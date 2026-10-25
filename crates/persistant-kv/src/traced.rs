// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Traced`]: a [`KvBackend`] that wraps another and emits one `tracing` span per call, with the
//! field names of [`persistant_core::telemetry`]. It changes nothing else.
//!
//! The span carries the kind, the backend's name, the operation, the capability id and, when the
//! call returns, its outcome (`ok` or the error's kind). It never carries a key, a value or an
//! error's text: keys can be secrets and error text can quote them. Spans are at `DEBUG`.

use crate::{BatchOp, BoundedProbe, KeyPage, KvBackend, KvError, ValuePage};
use std::future::Future;
use std::time::Duration;
use tracing::Instrument;

/// A [`KvBackend`] that emits a `persist` span around every call of the wrapped backend.
#[derive(Debug)]
pub struct Traced<K> {
    inner: K,
}

impl<K: KvBackend> Traced<K> {
    /// Wrap `inner`.
    pub fn new(inner: K) -> Self {
        Traced { inner }
    }

    /// The wrapped backend.
    pub fn into_inner(self) -> K {
        self.inner
    }

    async fn span<T>(
        &self,
        op: &'static str,
        capability: &'static str,
        call: impl Future<Output = Result<T, KvError>>,
    ) -> Result<T, KvError> {
        let span = tracing::debug_span!(
            "persist",
            persist.kind = "kv",
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
                Err(KvError::Backend(_)) => "backend",
                Err(KvError::Unsupported(_)) => "unsupported",
                Err(KvError::Corrupt(_)) => "corrupt",
                Err(KvError::TimedOut(_)) => "timed_out",
            },
        );
        r
    }
}

impl<K: KvBackend> KvBackend for Traced<K> {
    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn call_bound(&self) -> Option<Duration> {
        self.inner.call_bound()
    }

    fn bounded_call_probe(&self) -> impl Future<Output = Result<BoundedProbe, KvError>> + Send {
        self.span(
            "bounded_call_probe",
            "kv.bounded_call",
            self.inner.bounded_call_probe(),
        )
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.span("get", "kv.get", self.inner.get(key)).await
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        self.span("put", "kv.put", self.inner.put(key, value, ttl))
            .await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.span("delete", "kv.delete", self.inner.delete(key))
            .await
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.span(
            "put_if_absent",
            "kv.put_if_absent",
            self.inner.put_if_absent(key, value, ttl),
        )
        .await
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.span(
            "prefix_list",
            "kv.prefix_list",
            self.inner.prefix_list(prefix, after, limit),
        )
        .await
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.span(
            "ordered_scan",
            "kv.ordered_scan",
            self.inner.ordered_scan(prefix, after, limit),
        )
        .await
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        self.span(
            "compare_and_swap",
            "kv.compare_and_swap",
            self.inner.compare_and_swap(key, expected, new, ttl),
        )
        .await
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        self.span(
            "compare_and_delete",
            "kv.compare_and_delete",
            self.inner.compare_and_delete(key, expected),
        )
        .await
    }

    async fn take(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.span("take", "kv.take", self.inner.take(key)).await
    }

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        self.span("ttl_remaining", "kv.ttl", self.inner.ttl_remaining(key))
            .await
    }

    async fn purge_expired(&self) -> Result<u64, KvError> {
        self.span(
            "purge_expired",
            "kv.purge_expired",
            self.inner.purge_expired(),
        )
        .await
    }

    async fn count(&self, prefix: &str) -> Result<u64, KvError> {
        self.span("count", "kv.count", self.inner.count(prefix))
            .await
    }

    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        self.span("batch", "kv.batch_atomic", self.inner.batch(ops))
            .await
    }

    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        self.span(
            "ordered_scan_values",
            "kv.ordered_scan",
            self.inner.ordered_scan_values(prefix, after, limit),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKv;
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

    /// A backend whose `get` fails with the key in the message: the control for "no error text".
    struct Leaky;

    impl KvBackend for Leaky {
        fn backend(&self) -> &str {
            "leaky"
        }
        fn version(&self) -> &str {
            "0"
        }
        async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
            Err(KvError::Backend(format!("cannot read {key}")))
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: Option<Duration>) -> Result<(), KvError> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<(), KvError> {
            Ok(())
        }
        async fn put_if_absent(
            &self,
            _: &str,
            _: Vec<u8>,
            _: Option<Duration>,
        ) -> Result<bool, KvError> {
            Ok(true)
        }
        async fn prefix_list(
            &self,
            _: &str,
            _: Option<&str>,
            _: usize,
        ) -> Result<KeyPage, KvError> {
            Ok(KeyPage::new(vec![], None))
        }
        async fn ordered_scan(
            &self,
            _: &str,
            _: Option<&str>,
            _: usize,
        ) -> Result<KeyPage, KvError> {
            Ok(KeyPage::new(vec![], None))
        }
        async fn compare_and_swap(
            &self,
            _: &str,
            _: &[u8],
            _: Vec<u8>,
            _: Option<Duration>,
        ) -> Result<bool, KvError> {
            Ok(false)
        }
        async fn compare_and_delete(&self, _: &str, _: &[u8]) -> Result<bool, KvError> {
            Ok(false)
        }
    }

    #[test]
    fn the_field_names_are_the_documented_ones() {
        assert_eq!(telemetry::SPAN, "persist");
        assert_eq!(telemetry::KIND, "persist.kind");
        assert_eq!(telemetry::BACKEND, "persist.backend");
        assert_eq!(telemetry::OP, "persist.op");
        assert_eq!(telemetry::CAPABILITY, "persist.capability");
        assert_eq!(telemetry::OUTCOME, "persist.outcome");
    }

    #[tokio::test]
    async fn a_call_emits_one_span_with_the_shared_names_and_no_content() {
        let (spans, _guard) = captured();
        let kv = Traced::new(MemoryKv::new());
        kv.put("secret-key", b"secret-value".to_vec(), None)
            .await
            .unwrap();
        assert_eq!(
            kv.get("secret-key").await.unwrap().unwrap(),
            b"secret-value"
        );
        let spans = spans.lock().unwrap();
        assert_eq!(spans.len(), 2, "one span per call: {spans:?}");
        let ops: Vec<_> = spans.values().map(|f| f[telemetry::OP].as_str()).collect();
        assert_eq!(ops, ["put", "get"]);
        for f in spans.values() {
            assert_eq!(f["span.name"], telemetry::SPAN);
            assert_eq!(f[telemetry::KIND], "kv");
            assert_eq!(f[telemetry::BACKEND], kv.backend());
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
        let kv = Traced::new(Leaky);
        assert!(kv.get("secret-key").await.is_err());
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
        let e = Leaky.get("secret-key").await.unwrap_err();
        assert!(e.to_string().contains("secret-key"));
    }

    #[test]
    fn every_capability_named_is_in_the_vocabulary() {
        let src = include_str!("traced.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let named: Vec<&str> = code
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| s.starts_with("kv."))
            .collect();
        assert!(named.len() >= 15, "found only {named:?}");
        for id in named {
            assert!(
                vocabulary::SEED.iter().any(|(s, _)| *s == id),
                "{id} is not a seed capability"
            );
        }
    }
}
