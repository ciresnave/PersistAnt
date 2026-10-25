// SPDX-License-Identifier: MIT OR Apache-2.0
//! [`Traced`]: a [`RelationalBackend`] that wraps another and emits one `tracing` span per call
//! (and per transaction call), with the field names of [`persistant_core::telemetry`]. It changes
//! nothing else.
//!
//! The span carries the kind, the backend's name, the operation, the capability id and, when the
//! call returns, its outcome (`ok` or the error's kind). It never carries the SQL text, a
//! parameter, a row or an error's text: statements can hold literals, and constraint names and
//! driver messages can quote data. Spans are at `DEBUG`.

use crate::{Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tracing::Instrument;

async fn span<T>(
    backend: &str,
    op: &'static str,
    capability: &'static str,
    call: impl Future<Output = Result<T, RelError>>,
) -> Result<T, RelError> {
    let span = tracing::debug_span!(
        "persist",
        persist.kind = "rel",
        persist.backend = backend,
        persist.op = op,
        persist.capability = capability,
        persist.outcome = tracing::field::Empty,
    );
    let r = call.instrument(span.clone()).await;
    span.record(
        "persist.outcome",
        match &r {
            Ok(_) => "ok",
            Err(RelError::UniqueViolation { .. }) => "unique_violation",
            Err(RelError::ForeignKeyViolation { .. }) => "foreign_key_violation",
            Err(RelError::CheckViolation { .. }) => "check_violation",
            Err(RelError::Serialization) => "serialization",
            Err(RelError::Deadlock) => "deadlock",
            Err(RelError::Unavailable(_)) => "unavailable",
            Err(RelError::Unsupported(_)) => "unsupported",
            Err(RelError::Timeout { .. }) => "timeout",
            Err(RelError::Backend(_)) => "backend",
        },
    );
    r
}

/// A [`RelationalBackend`] that emits a `persist` span around every call of the wrapped backend and
/// of the transactions it begins.
#[derive(Debug)]
pub struct Traced<R> {
    inner: R,
}

impl<R: RelationalBackend> Traced<R> {
    /// Wrap `inner`.
    pub fn new(inner: R) -> Self {
        Traced { inner }
    }

    /// The wrapped backend.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// A transaction begun through [`Traced`]; its statements, commit and rollback are traced too.
#[derive(Debug)]
pub struct TracedTx<T> {
    inner: T,
    backend: String,
}

impl<R: RelationalBackend> RelationalBackend for Traced<R> {
    type Tx<'a>
        = TracedTx<R::Tx<'a>>
    where
        Self: 'a;

    fn backend(&self) -> &str {
        self.inner.backend()
    }

    fn version(&self) -> &str {
        self.inner.version()
    }

    fn dialect(&self) -> Dialect {
        self.inner.dialect()
    }

    async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        span(
            self.inner.backend(),
            "execute",
            "rel.values",
            self.inner.execute(sql, params),
        )
        .await
    }

    async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        span(
            self.inner.backend(),
            "query",
            "rel.values",
            self.inner.query(sql, params),
        )
        .await
    }

    async fn begin(&self, isolation: Isolation) -> Result<Self::Tx<'_>, RelError> {
        let inner = span(
            self.inner.backend(),
            "begin",
            "rel.transaction",
            self.inner.begin(isolation),
        )
        .await?;
        Ok(TracedTx {
            inner,
            backend: self.inner.backend().to_string(),
        })
    }

    fn call_bound(&self) -> Option<Duration> {
        self.inner.call_bound()
    }

    fn with_lock_held<'a>(
        &'a self,
        lock_sql: &'a str,
        at_most: Duration,
        during: Pin<Box<dyn Future<Output = ()> + Send + 'a>>,
    ) -> impl Future<Output = Result<(), RelError>> + Send {
        self.inner.with_lock_held(lock_sql, at_most, during)
    }
}

impl<T: RelationalTx> RelationalTx for TracedTx<T> {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        span(
            &self.backend,
            "tx.execute",
            "rel.values",
            self.inner.execute(sql, params),
        )
        .await
    }

    async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        span(
            &self.backend,
            "tx.query",
            "rel.values",
            self.inner.query(sql, params),
        )
        .await
    }

    async fn commit(self) -> Result<(), RelError> {
        span(
            &self.backend,
            "tx.commit",
            "rel.transaction",
            self.inner.commit(),
        )
        .await
    }

    async fn rollback(self) -> Result<(), RelError> {
        span(
            &self.backend,
            "tx.rollback",
            "rel.transaction",
            self.inner.rollback(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// A backend with no database: `execute` succeeds, `query` fails quoting the statement and
    /// `commit` fails with a unique violation naming a constraint. The control for "no text".
    struct Fake;
    struct FakeTx;

    impl RelationalBackend for Fake {
        type Tx<'a> = FakeTx;
        fn backend(&self) -> &str {
            "fake"
        }
        fn version(&self) -> &str {
            "0"
        }
        fn dialect(&self) -> Dialect {
            Dialect::Postgres
        }
        async fn execute(&self, _: &str, _: &[Value]) -> Result<u64, RelError> {
            Ok(1)
        }
        async fn query(&self, sql: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
            Err(RelError::Backend(format!("cannot run {sql}")))
        }
        async fn begin(&self, _: Isolation) -> Result<FakeTx, RelError> {
            Ok(FakeTx)
        }
    }

    impl RelationalTx for FakeTx {
        async fn execute(&mut self, _: &str, _: &[Value]) -> Result<u64, RelError> {
            Ok(1)
        }
        async fn query(&mut self, _: &str, _: &[Value]) -> Result<Vec<Row>, RelError> {
            Ok(vec![])
        }
        async fn commit(self) -> Result<(), RelError> {
            Err(RelError::UniqueViolation {
                constraint: Some("secret_constraint".to_string()),
            })
        }
        async fn rollback(self) -> Result<(), RelError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn statements_and_a_transaction_emit_spans_with_the_shared_names() {
        let (spans, _guard) = captured();
        let db = Traced::new(Fake);
        db.execute(
            "INSERT INTO secret_table VALUES ($1)",
            &[Value::Text("secret".into())],
        )
        .await
        .unwrap();
        let mut tx = db.begin(Isolation::default()).await.unwrap();
        tx.execute("UPDATE secret_table SET a = 1", &[])
            .await
            .unwrap();
        tx.rollback().await.unwrap();
        let spans = spans.lock().unwrap();
        let ops: Vec<_> = spans.values().map(|f| f[telemetry::OP].as_str()).collect();
        assert_eq!(ops, ["execute", "begin", "tx.execute", "tx.rollback"]);
        for f in spans.values() {
            assert_eq!(f["span.name"], telemetry::SPAN);
            assert_eq!(f[telemetry::KIND], "rel");
            assert_eq!(f[telemetry::BACKEND], "fake");
            assert_eq!(f[telemetry::OUTCOME], "ok");
            assert!(
                !format!("{f:?}").contains("secret"),
                "SQL or a parameter reached a field: {f:?}"
            );
        }
    }

    #[tokio::test]
    async fn an_error_records_its_kind_and_never_its_text() {
        let (spans, _guard) = captured();
        let db = Traced::new(Fake);
        assert!(db.query("SELECT secret_column", &[]).await.is_err());
        let tx = db.begin(Isolation::default()).await.unwrap();
        assert!(tx.commit().await.is_err());
        {
            let spans = spans.lock().unwrap();
            let outcomes: Vec<_> = spans
                .values()
                .map(|f| f[telemetry::OUTCOME].as_str())
                .collect();
            assert_eq!(outcomes, ["backend", "ok", "unique_violation"]);
            assert!(
                !format!("{spans:?}").contains("secret"),
                "error text leaked: {spans:?}"
            );
        }
        // Control: both errors do carry the text, so the check above could fail.
        let e = Fake.query("SELECT secret_column", &[]).await.unwrap_err();
        assert!(e.to_string().contains("secret_column"));
        let e = FakeTx.commit().await.unwrap_err();
        assert!(e.to_string().contains("secret_constraint"));
    }

    #[test]
    fn every_capability_named_is_in_the_vocabulary() {
        let src = include_str!("traced.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        let named: Vec<&str> = code
            .split('"')
            .skip(1)
            .step_by(2)
            .filter(|s| s.starts_with("rel."))
            .collect();
        assert!(named.len() >= 6, "found only {named:?}");
        for id in named {
            assert!(
                vocabulary::SEED.iter().any(|(s, _)| *s == id),
                "{id} is not a seed capability"
            );
        }
    }
}
