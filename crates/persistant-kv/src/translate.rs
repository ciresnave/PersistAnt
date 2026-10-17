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
        // A key is at most 1024 bytes on MySQL (an indexed `VARBINARY`; InnoDB allows 3072). The
        // comparison is bytewise, the trap a text column or a `utf8mb4_0900_ai_ci` key would fall
        // into. A longer key is rejected by the server (strict mode), not truncated.
        Dialect::MySql | Dialect::MariaDb => Some(
            "CREATE TABLE IF NOT EXISTS persistant_kv (             k VARBINARY(1024) NOT NULL PRIMARY KEY, v LONGBLOB NOT NULL, expires_ms BIGINT)              ENGINE=InnoDB",
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
    /// Placeholders are a bare `?`: values bind in `Translation::param_order`.
    positional: bool,
    opts: Options,
    cache: Mutex<HashMap<String, (String, Vec<u32>)>>,
}

impl Translator {
    pub(crate) fn new(dialect: Dialect) -> Result<Self, RelError> {
        let target = match &dialect {
            Dialect::Postgres => None,
            Dialect::Sqlite => Some("sqlite"),
            Dialect::MySql => Some("mysql"),
            Dialect::MariaDb => Some("mariadb"),
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
        // MySQL has no `ON CONFLICT .. WHERE`, and its "rows affected" cannot tell an insert from a
        // no-op under sqlx's `CLIENT_FOUND_ROWS`: `KvOnRelational::claim_two_step` avoids both.
        let opts = Options::default()
            .with(Allowance::LenientCast)
            .with(Allowance::ClockIsStatementStart)
            .with(Allowance::UpsertAnyUniqueKey)
            .with_schema(schema);
        Ok(Translator {
            positional: matches!(dialect, Dialect::MySql | Dialect::MariaDb),
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
    /// placeholders are numbered (`?1`), so the values go as written; MySQL's are positional `?`,
    /// so the values are reordered by `Translation::param_order`.
    pub(crate) fn prepare(
        &self,
        sql: &str,
        params: &[Value],
    ) -> Result<(String, Vec<Value>), RelError> {
        let Some(target) = self.target else {
            return Ok((sql.to_string(), params.to_vec()));
        };
        let cached = self.cache.lock().ok().and_then(|c| c.get(sql).cloned());
        let (out, order) = match cached {
            Some(hit) => hit,
            None => {
                let t = unisql::translate(sql, "postgres", target, &self.opts)
                    .map_err(|e| RelError::Unsupported(format!("unisql: {e}")))?;
                let hit = (t.sql, t.param_order);
                if let Ok(mut c) = self.cache.lock() {
                    c.insert(sql.to_string(), hit.clone());
                }
                hit
            }
        };
        if !self.positional {
            return Ok((out, params.to_vec()));
        }
        // A source parameter used twice appears twice, in placeholder order.
        let mut bound = Vec::with_capacity(order.len());
        for n in order {
            let v = usize::try_from(n)
                .ok()
                .and_then(|n| n.checked_sub(1))
                .and_then(|i| params.get(i))
                .ok_or_else(|| {
                    RelError::Backend(format!("statement uses parameter {n}, none given"))
                })?;
            bound.push(v.clone());
        }
        Ok((out, bound))
    }
}
