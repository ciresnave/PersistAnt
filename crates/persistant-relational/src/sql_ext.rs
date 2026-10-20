// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running a statement that was checked and translated at build time (`persistant::sql!`) on a
//! backend. Feature `sql`.
//!
//! The macro embeds the text for every target it was told about; here the backend's own
//! [`Dialect`] picks one, and the target's parameter order is applied (a target may repeat or reorder
//! placeholders). Nothing is parsed or translated at run time. The core traits do not change.

use crate::{Dialect, RelError, RelationalBackend, RelationalTx, Row, Value};
use std::future::Future;
use unisql_embed::{Binding, Query, Target};

/// A statement checked and translated at build time. Built by `persistant::sql!`.
pub type Sql = Query;

fn target_of(dialect: &Dialect) -> Option<Target> {
    match dialect {
        Dialect::Postgres => Some(Target::Postgres),
        Dialect::Sqlite => Some(Target::Sqlite),
        Dialect::MySql => Some(Target::Mysql),
        Dialect::MariaDb => Some(Target::Mariadb),
        Dialect::Other(_) => None,
    }
}

/// The text for `dialect` and the parameters in the order that text wants them.
fn prepare(
    sql: &Sql,
    dialect: &Dialect,
    params: &[Value],
) -> Result<(&'static str, Vec<Value>), RelError> {
    let rendered = target_of(dialect)
        .and_then(|t| sql.for_target(t))
        .ok_or_else(|| {
            RelError::Unsupported(format!(
                "the statement was not translated for {dialect}: name it in the macro's `targets`"
            ))
        })?;
    if params.len() != sql.params as usize {
        return Err(RelError::Backend(format!(
            "the statement takes {} parameters, got {}",
            sql.params,
            params.len()
        )));
    }
    if let Some(n) = rendered
        .param_bindings
        .iter()
        .position(|b| *b != Binding::Scalar)
    {
        return Err(RelError::Unsupported(format!(
            "parameter ${} is an array on {dialect}, which `Value` does not carry yet",
            n + 1
        )));
    }
    let ordered = rendered
        .param_order
        .iter()
        .map(|n| params[(*n - 1) as usize].clone())
        .collect();
    Ok((rendered.sql, ordered))
}

/// `execute_sql` and `query_sql` on every [`RelationalBackend`].
pub trait RelationalSqlExt: RelationalBackend {
    /// As [`RelationalBackend::execute`], with a statement checked at build time and `params` in the
    /// order of its `$1..$n` placeholders.
    fn execute_sql<'a>(
        &'a self,
        sql: &Sql,
        params: &[Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send + 'a {
        let prepared = prepare(sql, &self.dialect(), params);
        async move {
            let (text, ordered) = prepared?;
            self.execute(text, &ordered).await
        }
    }

    /// As [`RelationalBackend::query`], with a statement checked at build time.
    fn query_sql<'a>(
        &'a self,
        sql: &Sql,
        params: &[Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send + 'a {
        let prepared = prepare(sql, &self.dialect(), params);
        async move {
            let (text, ordered) = prepared?;
            self.query(text, &ordered).await
        }
    }
}

impl<T: RelationalBackend + ?Sized> RelationalSqlExt for T {}

/// `execute_sql` and `query_sql` inside a transaction. A transaction does not know its dialect, so
/// the caller passes the backend's (`db.dialect()`).
pub trait RelationalTxSqlExt: RelationalTx {
    /// As [`RelationalTx::execute`], with a statement checked at build time.
    fn execute_sql<'a>(
        &'a mut self,
        dialect: &Dialect,
        sql: &Sql,
        params: &[Value],
    ) -> impl Future<Output = Result<u64, RelError>> + Send + 'a {
        let prepared = prepare(sql, dialect, params);
        async move {
            let (text, ordered) = prepared?;
            self.execute(text, &ordered).await
        }
    }

    /// As [`RelationalTx::query`], with a statement checked at build time.
    fn query_sql<'a>(
        &'a mut self,
        dialect: &Dialect,
        sql: &Sql,
        params: &[Value],
    ) -> impl Future<Output = Result<Vec<Row>, RelError>> + Send + 'a {
        let prepared = prepare(sql, dialect, params);
        async move {
            let (text, ordered) = prepared?;
            self.query(text, &ordered).await
        }
    }
}

impl<T: RelationalTx + ?Sized> RelationalTxSqlExt for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Isolation;
    use std::sync::{Arc, Mutex};
    use unisql_embed::Rendered;

    /// `SELECT $2, $1, $1`: SQLite wants the numbered form, MySQL positional placeholders in the order
    /// they appear.
    const Q: Sql = Query {
        params: 2,
        constructs: &[],
        rendered: &[
            Rendered {
                target: Target::Sqlite,
                sql: "SELECT ?2, ?1, ?1",
                param_order: &[2, 1, 1],
                param_bindings: &[Binding::Scalar, Binding::Scalar],
            },
            Rendered {
                target: Target::Mysql,
                sql: "SELECT ?, ?, ?",
                param_order: &[2, 1, 1],
                param_bindings: &[Binding::Scalar, Binding::Scalar],
            },
            Rendered {
                target: Target::Postgres,
                sql: "SELECT $2, $1, $1",
                param_order: &[1, 2],
                param_bindings: &[Binding::Scalar, Binding::NativeArray],
            },
        ],
    };

    type Call = (String, Vec<Value>);

    #[derive(Clone, Default)]
    struct Seen(Arc<Mutex<Vec<Call>>>);

    struct Fake {
        dialect: Dialect,
        seen: Seen,
    }

    struct FakeTx(Seen);

    impl RelationalBackend for Fake {
        type Tx<'a> = FakeTx;
        fn backend(&self) -> &str {
            "fake"
        }
        fn version(&self) -> &str {
            "0"
        }
        fn dialect(&self) -> Dialect {
            self.dialect.clone()
        }
        async fn execute(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
            self.seen
                .0
                .lock()
                .unwrap()
                .push((sql.into(), params.to_vec()));
            Ok(1)
        }
        async fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
            self.seen
                .0
                .lock()
                .unwrap()
                .push((sql.into(), params.to_vec()));
            Ok(Vec::new())
        }
        async fn begin(&self, _: Isolation) -> Result<FakeTx, RelError> {
            Ok(FakeTx(self.seen.clone()))
        }
    }

    impl RelationalTx for FakeTx {
        async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
            self.0.0.lock().unwrap().push((sql.into(), params.to_vec()));
            Ok(1)
        }
        async fn query(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
            self.0.0.lock().unwrap().push((sql.into(), params.to_vec()));
            Ok(Vec::new())
        }
        async fn commit(self) -> Result<(), RelError> {
            Ok(())
        }
        async fn rollback(self) -> Result<(), RelError> {
            Ok(())
        }
    }

    fn fake(dialect: Dialect) -> Fake {
        Fake {
            dialect,
            seen: Seen::default(),
        }
    }

    fn block<T>(f: impl Future<Output = T>) -> T {
        // A fake that never waits: one poll finishes it.
        let mut f = std::pin::pin!(f);
        let waker = std::task::Waker::noop();
        match f.as_mut().poll(&mut std::task::Context::from_waker(waker)) {
            std::task::Poll::Ready(v) => v,
            std::task::Poll::Pending => panic!("the fake backend waited"),
        }
    }

    #[test]
    fn the_backends_own_text_and_parameter_order_are_used() {
        let a = Value::Int(10);
        let b = Value::Text("x".into());
        let db = fake(Dialect::Sqlite);
        block(db.execute_sql(&Q, &[a.clone(), b.clone()])).unwrap();
        let db2 = fake(Dialect::MySql);
        block(db2.query_sql(&Q, &[a.clone(), b.clone()])).unwrap();
        // $2, $1, $1 for both
        let want = vec![b.clone(), a.clone(), a.clone()];
        assert_eq!(
            db.seen.0.lock().unwrap()[0],
            ("SELECT ?2, ?1, ?1".to_string(), want.clone())
        );
        assert_eq!(
            db2.seen.0.lock().unwrap()[0],
            ("SELECT ?, ?, ?".to_string(), want)
        );
    }

    #[test]
    fn a_dialect_the_call_did_not_name_is_unsupported_not_guessed() {
        let q: Sql = Query {
            params: 0,
            constructs: &[],
            rendered: &[Rendered {
                target: Target::Sqlite,
                sql: "SELECT 1",
                param_order: &[],
                param_bindings: &[],
            }],
        };
        // The control: the named dialect runs.
        assert!(block(fake(Dialect::Sqlite).execute_sql(&q, &[])).is_ok());
        for d in [
            Dialect::Postgres,
            Dialect::MariaDb,
            Dialect::Other("x".into()),
        ] {
            match block(fake(d).execute_sql(&q, &[])) {
                Err(RelError::Unsupported(m)) => assert!(m.contains("targets"), "{m}"),
                other => panic!("expected Unsupported, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_wrong_number_of_parameters_is_an_error() {
        let db = fake(Dialect::Sqlite);
        let one = [Value::Int(1)];
        assert!(matches!(
            block(db.execute_sql(&Q, &one)),
            Err(RelError::Backend(m)) if m.contains("takes 2 parameters, got 1")
        ));
        assert!(db.seen.0.lock().unwrap().is_empty(), "nothing ran");
        assert!(block(db.execute_sql(&Q, &[Value::Int(1), Value::Int(2)])).is_ok());
    }

    #[test]
    fn an_array_parameter_is_refused_until_value_carries_one() {
        let db = fake(Dialect::Postgres);
        match block(db.query_sql(&Q, &[Value::Int(1), Value::Int(2)])) {
            Err(RelError::Unsupported(m)) => assert!(m.contains("$2"), "{m}"),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(db.seen.0.lock().unwrap().is_empty());
    }

    #[test]
    fn a_transaction_takes_the_dialect_from_the_caller() {
        let db = fake(Dialect::MySql);
        let mut tx = block(db.begin(Isolation::ReadCommitted)).unwrap();
        block(tx.execute_sql(&db.dialect(), &Q, &[Value::Int(1), Value::Int(2)])).unwrap();
        assert_eq!(db.seen.0.lock().unwrap()[0].0, "SELECT ?, ?, ?");
        assert!(matches!(
            block(tx.query_sql(
                &Dialect::Other("x".into()),
                &Q,
                &[Value::Int(1), Value::Int(2)]
            )),
            Err(RelError::Unsupported(_))
        ));
    }
}
