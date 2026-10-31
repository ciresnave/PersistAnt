// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded-memory test 6 (docs/DESIGN-BLOB-STREAMS.md section 4): 256 MiB copied through `copy_in`
//! with an 8 MiB chunk into an `fs` sink stays under a stated peak, measured by this binary's own
//! counting allocator rather than host RAM. The known-bad control buffers the whole value and must
//! exceed the same peak, so the limit is shown to be able to fail.
//!
//! The allocator is global, so this file holds exactly one test: nothing else may allocate between
//! the counter reset and the read of the peak.

use persistant_blob::BlobBackend;
use persistant_blob::stream::copy_in;
use persistant_opendal::OpendalBlob;
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    let now = CURRENT.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        unsafe { System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new >= l.size() {
            grow(new - l.size());
        } else {
            CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
        }
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const VALUE: u64 = 256 << 20;
const CHUNK: usize = 8 << 20;
/// One chunk buffer plus generous room for the runtime, the sink and the file layer.
const PEAK_LIMIT: usize = 24 << 20;

/// A source that produces `left` bytes without holding them.
struct Generated {
    left: u64,
}

impl Read for Generated {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        buf[..n].fill(0xAB);
        self.left -= n as u64;
        Ok(n)
    }
}

fn reset_peak() {
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
}

fn peak_since_reset() -> usize {
    PEAK.load(Ordering::Relaxed)
}

#[tokio::test]
async fn copy_in_of_256_mib_stays_under_the_peak_and_whole_value_buffering_does_not() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();

    reset_peak();
    let before = CURRENT.load(Ordering::Relaxed);
    let mut sink = b.write_stream("streamed").await.unwrap();
    let copied = copy_in(&mut *sink, Generated { left: VALUE }, CHUNK)
        .await
        .unwrap();
    sink.finish().await.unwrap();
    let streamed_peak = peak_since_reset() - before;
    assert_eq!(copied, VALUE);
    assert_eq!(
        std::fs::metadata(root.path().join("streamed"))
            .unwrap()
            .len(),
        VALUE,
        "the streamed value is not whole"
    );
    assert!(
        streamed_peak <= PEAK_LIMIT,
        "copy_in peaked at {streamed_peak} bytes, over the {PEAK_LIMIT} limit"
    );

    // Known-bad control: the same copy, but the whole value is buffered first.
    reset_peak();
    let before = CURRENT.load(Ordering::Relaxed);
    let mut whole = Vec::new();
    Generated { left: VALUE }.read_to_end(&mut whole).unwrap();
    let mut sink = b.write_stream("buffered").await.unwrap();
    sink.write(&whole).await.unwrap();
    sink.finish().await.unwrap();
    let buffered_peak = peak_since_reset() - before;
    drop(whole);
    assert!(
        buffered_peak > PEAK_LIMIT,
        "control broken: buffering the whole value peaked at only {buffered_peak} bytes"
    );
}
