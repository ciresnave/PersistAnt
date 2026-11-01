// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Traced`]: a [`BlobBackend`] that wraps another and emits one `tracing` span per call, with the
//! field names of [`persistant_core::telemetry`]. It changes nothing else.
//!
//! The span carries the kind, the backend's name, the operation, the capability id and, when the
//! call returns, its outcome (`ok` or the error's kind). It never carries a key, a value or an
//! error's text: keys can be secrets and error text can quote them. Spans are at `DEBUG`.

use crate::{BlobBackend, BlobError, BlobSink, BoxFuture, Entry, KeyLayout, Page};
use std::future::Future;
use tracing::Instrument;

/// A [`BlobBackend`] that emits a `persist` span around every call of the wrapped backend.
#[derive(Debug)]
pub struct Traced<B> {
    inner: B,
}

impl<B: BlobBackend> Traced<B> {
    /// Wrap `inner`.
    pub fn new(inner: B) -> Self {
        Traced { inner }
    }

    /// The wrapped backend.
    pub fn into_inner(self) -> B {
        self.inner
    }

    async fn span<T>(
        &self,
        op: &'static str,
        capability: &'static str,
        call: impl Future<Output = Result<T, BlobError>>,
    ) -> Result<T, BlobError> {
        let span = tracing::debug_span!(
            "persist",
            persist.kind = "blob",
            persist.backend = self.inner.backend(),
            persist.op = op,
            persist.capability = capability,
            persist.outcome = tracing::field::Empty,
        );
        let r = call.instrument(span.clone()).await;
        span.record(
            "persist.outcome",
            match &r {
                Ok(_) => "ok",
                Err(BlobError::Backend(_)) => "backend",
                Err(BlobError::Unsupported(_)) => "unsupported",
                Err(BlobError::Changed(_)) => "changed",
            },
        );
        r
    }
}

/// A [`BlobSink`] whose every call is inside a `persist` span, like the calls that opened it.
struct TracedSink {
    inner: Box<dyn BlobSink>,
    backend: String,
    capability: &'static str,
}

fn sink_span(backend: &str, op: &'static str, capability: &'static str) -> tracing::Span {
    tracing::debug_span!(
        "persist",
        persist.kind = "blob",
        persist.backend = backend,
        persist.op = op,
        persist.capability = capability,
        persist.outcome = tracing::field::Empty,
    )
}

fn record_outcome(span: &tracing::Span, r: &Result<(), BlobError>) {
    span.record(
        "persist.outcome",
        match r {
            Ok(()) => "ok",
            Err(BlobError::Backend(_)) => "backend",
            Err(BlobError::Unsupported(_)) => "unsupported",
            Err(BlobError::Changed(_)) => "changed",
        },
    );
}

impl BlobSink for TracedSink {
    fn write<'a>(&'a mut self, chunk: &'a [u8]) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let span = sink_span(&self.backend, "stream_write", self.capability);
            let r = self.inner.write(chunk).instrument(span.clone()).await;
            record_outcome(&span, &r);
            r
        })
    }

    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            let span = sink_span(&self.backend, "stream_finish", self.capability);
            let r = self.inner.finish().instrument(span.clone()).await;
            record_outcome(&span, &r);
            r
        })
    }

    fn abort(self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            let span = sink_span(&self.backend, "stream_abort", self.capability);
            let r = self.inner.abort().instrument(span.clone()).await;
            record_outcome(&span, &r);
            r
        })
    }
}

impl<B: BlobBackend> BlobBackend for Traced<B> {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn max_value_size(&self) -> Option<u64> {
        self.inner.max_value_size()
    }

    fn key_layout(&self) -> KeyLayout {
        self.inner.key_layout()
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        self.span("read", "blob.read", self.inner.read(key)).await
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.span("write", "blob.write", self.inner.write(key, bytes))
            .await
    }

    async fn delete(&self, key: &str) -> Result<(), BlobError> {
        self.span("delete", "blob.delete", self.inner.delete(key))
            .await
    }

    async fn stat(&self, key: &str) -> Result<Option<Entry>, BlobError> {
        self.span("stat", "blob.stat", self.inner.stat(key)).await
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page, BlobError> {
        self.span("list", "blob.list", self.inner.list(prefix, after, limit))
            .await
    }

    async fn create_exclusive(&self, key: &str, bytes: Vec<u8>) -> Result<bool, BlobError> {
        self.span(
            "create_exclusive",
            "blob.create_exclusive",
            self.inner.create_exclusive(key, bytes),
        )
        .await
    }

    async fn replace_atomic(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.span(
            "replace_atomic",
            "blob.replace_atomic",
            self.inner.replace_atomic(key, bytes),
        )
        .await
    }

    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.span("append", "blob.append", self.inner.append(key, bytes))
            .await
    }
    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        self.span(
            "read_range",
            "blob.read_range",
            self.inner.read_range(key, offset, len),
        )
        .await
    }

    async fn read_range_if(
        &self,
        key: &str,
        offset: u64,
        len: u64,
        tag: &str,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        self.span(
            "read_range_if",
            "blob.read_range.consistent",
            self.inner.read_range_if(key, offset, len, tag),
        )
        .await
    }

    async fn write_stream(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        let inner = self
            .span(
                "write_stream",
                "blob.write_stream",
                self.inner.write_stream(key),
            )
            .await?;
        Ok(Box::new(TracedSink {
            inner,
            backend: self.inner.backend().to_string(),
            capability: "blob.write_stream",
        }))
    }

    async fn write_stream_atomic(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        let inner = self
            .span(
                "write_stream_atomic",
                "blob.write_stream.atomic",
                self.inner.write_stream_atomic(key),
            )
            .await?;
        Ok(Box::new(TracedSink {
            inner,
            backend: self.inner.backend().to_string(),
            capability: "blob.write_stream.atomic",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlob;
    use persistant_core::{telemetry, vocabulary};
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, registry::LookupSpan};

    type Spans = Arc<Mutex<BTreeMap<u64, BTreeMap<String, String>>>>;

    #[derive(Default)]
    struct Fields(BTreeMap<String, String>);

    impl Visit for Fields {
        fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
            self.0.insert(f.name().to_string(), format!("{v:?}"));
        }
        fn record_str(&mut self, f: &Field, v: &str) {
            self.0.insert(f.name().to_string(), v.to_string());
        }
    }

    struct Capture(Spans);

    impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
        fn on_new_span(&self, a: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
            let mut f = Fields::default();
            a.record(&mut f);
            f.0.insert("span.name".into(), a.metadata().name().into());
            self.0.lock().unwrap().insert(id.into_u64(), f.0);
        }
        fn on_record(&self, id: &Id, r: &Record<'_>, _: Context<'_, S>) {
            let mut f = Fields::default();
            r.record(&mut f);
            if let Some(m) = self.0.lock().unwrap().get_mut(&id.into_u64()) {
                m.extend(f.0);
            }
        }
    }

    fn captured() -> (Spans, tracing::subscriber::DefaultGuard) {
        let spans = Spans::default();
        let sub = tracing_subscriber::registry().with(Capture(spans.clone()));
        (spans, tracing::subscriber::set_default(sub))
    }

    /// A backend whose `read` fails with the key in the message: the control for "no error text".
    struct Leaky;

    impl BlobBackend for Leaky {
        fn backend(&self) -> &str {
            "leaky"
        }
        fn version(&self) -> &str {
            "0"
        }
        async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
            Err(BlobError::Backend(format!("cannot read {key}")))
        }
        async fn write(&self, _: &str, _: Vec<u8>) -> Result<(), BlobError> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<(), BlobError> {
            Ok(())
        }
        async fn stat(&self, _: &str) -> Result<Option<Entry>, BlobError> {
            Ok(None)
        }
        async fn list(&self, _: &str, _: Option<&str>, _: usize) -> Result<Page, BlobError> {
            Ok(Page::new(vec![], None))
        }
    }

    #[tokio::test]
    async fn a_call_emits_one_span_with_the_shared_names_and_no_content() {
        let (spans, _guard) = captured();
        let b = Traced::new(MemoryBlob::new());
        b.write("secret-key", b"secret-value".to_vec())
            .await
            .unwrap();
        assert_eq!(
            b.read("secret-key").await.unwrap().unwrap(),
            b"secret-value"
        );
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 2, "one span per call: {spans:?}");
        let ops: Vec<_> = spans.values().map(|f| f[telemetry::OP].as_str()).collect();
        assert_eq!(ops, ["write", "read"]);
        for f in spans.values() {
            assert_eq!(f["span.name"], telemetry::SPAN);
            assert_eq!(f[telemetry::KIND], "blob");
            assert_eq!(f[telemetry::BACKEND], b.backend());
            assert_eq!(f[telemetry::OUTCOME], "ok");
            let all = format!("{f:?}");
            assert!(
                !all.contains("secret"),
                "a key or value reached a field: {all}"
            );
        }
    }

    #[tokio::test]
    async fn an_error_records_its_kind_and_never_its_text() {
        let (spans, _guard) = captured();
        let b = Traced::new(Leaky);
        assert!(b.read("secret-key").await.is_err());
        {
            let spans = spans.lock().unwrap();
            let f = spans.values().next().expect("a span");
            assert_eq!(f[telemetry::OUTCOME], "backend");
            assert!(
                !format!("{f:?}").contains("secret"),
                "error text leaked: {f:?}"
            );
        }
        // Control: the backend's own error does quote the key, so the check above could fail.
        let e = Leaky.read("secret-key").await.unwrap_err();
        assert!(e.to_string().contains("secret-key"));
    }

    #[tokio::test]
    async fn an_unsupported_operation_is_recorded_as_such() {
        let (spans, _guard) = captured();
        let b = Traced::new(Leaky);
        assert!(b.append("k", vec![1]).await.is_err());
        let spans = spans.lock().unwrap();
        assert_eq!(
            spans.values().next().unwrap()[telemetry::OUTCOME],
            "unsupported"
        );
    }

    #[test]
    fn every_capability_named_is_in_the_vocabulary() {
        let src = include_str!("traced.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let named: Vec<&str> = code
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| s.starts_with("blob."))
            .collect();
        assert!(named.len() >= 8, "found only {named:?}");
        for id in named {
            assert!(
                vocabulary::SEED.iter().any(|(s, _)| *s == id),
                "{id} is not a seed capability"
            );
        }
    }
}
