// SPDX-License-Identifier: MIT OR Apache-2.0
//! Helpers over the ranged and streamed operations of [`BlobBackend`]: a chunked reader that
//! notices a replacement, and a copy from a `Read` into a [`BlobSink`]. Not trait methods, so
//! there is one thing to certify (`docs/DESIGN-BLOB-STREAMS.md` 3.2).

use crate::{BlobBackend, BlobError, BlobSink};
use std::io::Read;

/// Reads one value in chunks with [`BlobBackend::read_range`], holding memory to one chunk.
///
/// If the backend gives a version tag ([`Entry::tag`](crate::Entry::tag)), every chunk is read with
/// [`read_range_if`](BlobBackend::read_range_if) and the tag is read again at the end, so a
/// replacement between chunks is [`BlobError::Changed`]. **With no tag only a change of length is
/// detected**: a same-length replacement mid-read is not, and the caller who needs one version
/// holds a lock of its own (3.7).
pub struct ChunkReader<'a, B> {
    backend: &'a B,
    key: String,
    tag: Option<String>,
    len: u64,
    offset: u64,
    chunk: u64,
}

impl<'a, B: BlobBackend> ChunkReader<'a, B> {
    /// Begin reading `key` in chunks of `chunk` bytes (at least 1); `None` if the key is absent.
    pub async fn new(
        backend: &'a B,
        key: &str,
        chunk: usize,
    ) -> Result<Option<ChunkReader<'a, B>>, BlobError> {
        let Some(e) = backend.stat(key).await? else {
            return Ok(None);
        };
        Ok(Some(ChunkReader {
            backend,
            key: key.to_string(),
            tag: e.tag,
            len: e.len,
            offset: 0,
            chunk: chunk.max(1) as u64,
        }))
    }

    /// The length the value had when the read began.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the value was empty when the read began.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The next chunk, or `None` at the end (after checking the value is still the one begun).
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, BlobError> {
        let changed = || BlobError::Changed(self.key.clone());
        if self.offset >= self.len {
            let now = self.backend.stat(&self.key).await?.ok_or_else(changed)?;
            if now.len != self.len || (self.tag.is_some() && now.tag != self.tag) {
                return Err(changed());
            }
            return Ok(None);
        }
        let want = self.chunk.min(self.len - self.offset);
        let got = match &self.tag {
            Some(t) => {
                self.backend
                    .read_range_if(&self.key, self.offset, want, t)
                    .await?
            }
            None => {
                self.backend
                    .read_range(&self.key, self.offset, want)
                    .await?
            }
        };
        match got {
            Some(b) if b.len() as u64 == want => {
                self.offset += want;
                Ok(Some(b))
            }
            // Absent, or shorter than the length stat gave: the value changed under the reader.
            _ => Err(changed()),
        }
    }
}

/// Copy `reader` into `sink` in pieces of `chunk` bytes (at least 1) and return the bytes copied.
/// Memory is one piece. The sink is left open: the caller calls `finish` or `abort`.
pub async fn copy_in<R: Read>(
    sink: &mut dyn BlobSink,
    mut reader: R,
    chunk: usize,
) -> Result<u64, BlobError> {
    let mut buf = vec![0u8; chunk.max(1)];
    let mut total = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(BlobError::Backend(format!("reading the source: {e}"))),
        };
        sink.write(&buf[..n]).await?;
        total += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defect, MemoryBlob};

    fn pattern(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    async fn drain(r: &mut ChunkReader<'_, MemoryBlob>) -> Result<Vec<u8>, BlobError> {
        let mut out = Vec::new();
        while let Some(c) = r.next().await? {
            out.extend(c);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn chunks_in_order_make_the_whole_value() {
        let b = MemoryBlob::new();
        let v = pattern(10_007, 3);
        b.write("k", v.clone()).await.unwrap();
        let mut r = ChunkReader::new(&b, "k", 1000).await.unwrap().unwrap();
        assert_eq!(r.len(), 10_007);
        assert_eq!(drain(&mut r).await.unwrap(), v);
        // An absent key is None, and an empty value reads as no chunks.
        assert!(ChunkReader::new(&b, "nope", 10).await.unwrap().is_none());
        b.write("e", Vec::new()).await.unwrap();
        let mut e = ChunkReader::new(&b, "e", 10).await.unwrap().unwrap();
        assert!(e.is_empty());
        assert_eq!(drain(&mut e).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn a_same_length_replacement_between_chunks_is_changed() {
        let b = MemoryBlob::new();
        b.write("k", pattern(5000, 1)).await.unwrap();
        let mut r = ChunkReader::new(&b, "k", 1000).await.unwrap().unwrap();
        assert!(r.next().await.unwrap().is_some());
        b.write("k", pattern(5000, 2)).await.unwrap();
        assert!(matches!(r.next().await, Err(BlobError::Changed(_))));
    }

    #[tokio::test]
    async fn the_known_bad_tag_lets_a_same_length_replacement_through() {
        // Control: with a length-only tag the same sequence is not noticed, so the test above can fail.
        let b = MemoryBlob::with_defect(Defect::TagIgnoresContent);
        b.write("k", pattern(5000, 1)).await.unwrap();
        let mut r = ChunkReader::new(&b, "k", 1000).await.unwrap().unwrap();
        assert!(r.next().await.unwrap().is_some());
        b.write("k", pattern(5000, 2)).await.unwrap();
        assert!(drain(&mut r).await.is_ok());
    }

    #[tokio::test]
    async fn a_shorter_replacement_is_changed_even_without_a_tag_match() {
        let b = MemoryBlob::new();
        b.write("k", pattern(5000, 1)).await.unwrap();
        let mut r = ChunkReader::new(&b, "k", 1000).await.unwrap().unwrap();
        assert!(r.next().await.unwrap().is_some());
        b.write("k", pattern(10, 1)).await.unwrap();
        assert!(matches!(r.next().await, Err(BlobError::Changed(_))));
    }

    #[tokio::test]
    async fn copy_in_streams_a_reader_into_a_sink_in_pieces() {
        let b = MemoryBlob::new();
        let v = pattern(100_003, 5);
        let mut sink = b.write_stream("k").await.unwrap();
        let n = copy_in(sink.as_mut(), std::io::Cursor::new(v.clone()), 4096)
            .await
            .unwrap();
        assert_eq!(n, v.len() as u64);
        assert_eq!(
            b.read("k").await.unwrap(),
            None,
            "not visible before finish"
        );
        sink.finish().await.unwrap();
        assert_eq!(b.read("k").await.unwrap(), Some(v));
    }

    #[tokio::test]
    async fn copy_in_reports_a_failing_source() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("disk gone"))
            }
        }
        let b = MemoryBlob::new();
        let mut sink = b.write_stream("k").await.unwrap();
        let e = copy_in(sink.as_mut(), Broken, 16).await.unwrap_err();
        assert!(matches!(e, BlobError::Backend(m) if m.contains("disk gone")));
    }
    const NEW_LEAVES: [crate::conformance::Leaf; 4] = [
        crate::conformance::Leaf::ReadRange,
        crate::conformance::Leaf::ReadRangeConsistent,
        crate::conformance::Leaf::WriteStream,
        crate::conformance::Leaf::WriteStreamAtomic,
    ];

    #[tokio::test]
    async fn the_instrumented_wrapper_forwards_the_new_operations() {
        let inner = MemoryBlob::new();
        let w = crate::stats::Instrumented::new(&inner);
        for leaf in NEW_LEAVES {
            crate::conformance::run_leaf(leaf, &w)
                .await
                .unwrap_or_else(|e| panic!("{}: {e:?}", leaf.id()));
        }
        let counts = w.counts();
        for op in [
            "read_range",
            "read_range_if",
            "write_stream",
            "write_stream_atomic",
        ] {
            assert!(counts.get(op).is_some_and(|c| c.ok > 0), "{op}: {counts:?}");
            assert_eq!(counts[op].unsupported, 0, "{op}");
        }
    }

    #[cfg(feature = "tracing")]
    #[tokio::test]
    async fn the_traced_wrapper_forwards_the_new_operations() {
        let w = crate::Traced::new(MemoryBlob::new());
        for leaf in NEW_LEAVES {
            crate::conformance::run_leaf(leaf, &w)
                .await
                .unwrap_or_else(|e| panic!("{}: {e:?}", leaf.id()));
        }
    }

    #[tokio::test]
    async fn a_wrapper_that_does_not_forward_fails_with_unsupported() {
        // Known-bad control: it implements only the core operations, so the defaults answer.
        struct NoForward(MemoryBlob);
        impl BlobBackend for NoForward {
            fn backend(&self) -> &str {
                self.0.backend()
            }
            fn version(&self) -> &str {
                self.0.version()
            }
            async fn read(&self, k: &str) -> Result<Option<Vec<u8>>, BlobError> {
                self.0.read(k).await
            }
            async fn write(&self, k: &str, b: Vec<u8>) -> Result<(), BlobError> {
                self.0.write(k, b).await
            }
            async fn delete(&self, k: &str) -> Result<(), BlobError> {
                self.0.delete(k).await
            }
            async fn stat(&self, k: &str) -> Result<Option<crate::Entry>, BlobError> {
                self.0.stat(k).await
            }
            async fn list(
                &self,
                p: &str,
                a: Option<&str>,
                l: usize,
            ) -> Result<crate::Page, BlobError> {
                self.0.list(p, a, l).await
            }
        }
        let w = NoForward(MemoryBlob::new());
        for leaf in NEW_LEAVES {
            let r = crate::conformance::run_leaf(leaf, &w).await;
            assert!(
                matches!(
                    r,
                    Err(crate::conformance::Failure::Backend(
                        BlobError::Unsupported(_)
                    ))
                ),
                "{}: {r:?}",
                leaf.id()
            );
        }
    }
}
