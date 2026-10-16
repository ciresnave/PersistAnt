// SPDX-License-Identifier: MIT OR Apache-2.0
//! Statements are written once, in PostgreSQL SQL, and translated for the target by UniSQL
//! (`docs/DESIGN-KINDS.md` §5). A construct UniSQL refuses is an error here, never a fallback.

use persistant_relational::{Dialect, RelError, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use unisql::{Allowance, Options};

/// The one table, per dialect. DDL is outside UniSQL's IR, so it is written here, once per dialect.
pub(crate) fn ddl(d: &Dialect) -> Option<&'static str> {
    match d {
        Dialect::Postgres => Some(
            "CREATE TABLE IF NOT EXISTS persistant_kv (\
             k BYTEA PRIMARY KEY, v BYTEA NOT NULL, expires_ms BIGINT)",
        ),
        Dialect::Sqlite => Some(
            "CREATE TABLE IF NOT EXISTS persistant_kv (\
             k BLOB PRIMARY KEY, v BLOB NOT NULL, expires_ms INTEGER)",
        ),
        _ => None,
    }
}

/// The schema the translator is told about: keys and values are bytes, never text.
const SCHEMA_DDL: &str =
    "CREATE TABLE persistant_kv (k BYTEA PRIMARY KEY, v BYTEA NOT NULL, expires_ms BIGINT)";

#[derive(Debug)]
pub(crate) struct Translator {
    dialect: Dialect,
    target: Option<&'static str>,
    opts: Options,
    cache: Mutex<HashMap<String, String>>,
}

impl Translator {
    pub(crate) fn new(dialect: Dialect) -> Result<Self, RelError> {
        let target = match &dialect {
            Dialect::Postgres => None,
            Dialect::Sqlite => Some("sqlite"),
            other => {
                return Err(RelError::Unsupported(format!(
                    "KvOnRelational has no translation to {other} yet"
                )));
            }
        };
        let schema = unisql::schema_from_ddl(SCHEMA_DDL, "postgres")
            .map_err(|e| RelError::Unsupported(format!("schema: {e}")))?
            .schema;
        // Each allowance is a named assumption, granted here and nowhere else:
        // - LenientCast: the only CAST is `EXTRACT(EPOCH ..) * 1000` to an integer, a number.
        // - ClockIsStatementStart: SQLite reads the clock once per statement; expiry only needs
        //   millisecond agreement across clients, not within one statement.
        // - UpsertAnyUniqueKey: `k` is the only unique key of the table.
        let opts = Options::default()
            .with(Allowance::LenientCast)
            .with(Allowance::ClockIsStatementStart)
            .with(Allowance::UpsertAnyUniqueKey)
            .with_schema(schema);
        Ok(Translator {
            dialect,
            target,
            opts,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn dialect(&self) -> &Dialect {
        &self.dialect
    }

    /// The statement and parameters to send. PostgreSQL is passed through unchanged. SQLite's
    /// placeholders are numbered (`?1`), so the values go as written; a dialect with positional
    /// `?` would have to bind in `Translation::param_order`, and none is supported yet.
    pub(crate) fn prepare(
        &self,
        sql: &str,
        params: &[Value],
    ) -> Result<(String, Vec<Value>), RelError> {
        let Some(target) = self.target else {
            return Ok((sql.to_string(), params.to_vec()));
        };
        let cached = self.cache.lock().ok().and_then(|c| c.get(sql).cloned());
        let out = match cached {
            Some(hit) => hit,
            None => {
                let t = unisql::translate(sql, "postgres", target, &self.opts)
                    .map_err(|e| RelError::Unsupported(format!("unisql: {e}")))?;
                if let Ok(mut c) = self.cache.lock() {
                    c.insert(sql.to_string(), t.sql.clone());
                }
                t.sql
            }
        };
        Ok((out, params.to_vec()))
    }
}
