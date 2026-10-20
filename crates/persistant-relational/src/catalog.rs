// SPDX-License-Identifier: MIT OR Apache-2.0
//! A schema snapshot: what a database's catalog says about its tables, read through the
//! [`RelationalBackend`] trait on PostgreSQL, MySQL/MariaDB and SQLite, and a deterministic JSON file
//! to commit beside the code (`docs/COMPILE-TIME-SQL.md` section 1). Feature `catalog`.
//!
//! The file records structure only, never data. Tables are base tables of the connection's current
//! schema or database; the migration ledger `_sqlx_migrations` is never in it.
//!
//! What each engine can tell us, and how it is normalised:
//! * `declared_type` is the engine's own spelling, lower-cased (`bigint`, `character varying`,
//!   `varchar(40)`, `integer`). Types are not mapped across engines.
//! * `nullable` is the column's `NOT NULL` flag inverted. SQLite is the exception that proves why it
//!   is read with care: only an `INTEGER PRIMARY KEY` is truly never NULL there, but a primary-key
//!   column is reported as not nullable on every engine, which is the declared intent.
//! * `has_default` means "the column may be left out of an `INSERT`": a `DEFAULT`, an identity or
//!   auto-increment column, or SQLite's rowid alias. A MariaDB `COLUMN_DEFAULT` of the text `NULL`
//!   (how it reports a nullable column with no default) is not a default.
//! * Keys are the primary key and the unique constraints or unique indexes over plain columns.
//!   Partial and expression indexes are skipped: they do not make a conflict target.

use crate::{Dialect, RelError, RelationalBackend, Row, Value};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

/// The snapshot file format this code writes and reads. A file with a larger number is refused, not
/// guessed at.
pub const FORMAT: u32 = 1;

/// One column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// The column name.
    pub name: String,
    /// The engine's own type name, lower-cased.
    #[serde(rename = "type")]
    pub declared_type: String,
    /// Whether NULL is allowed.
    pub nullable: bool,
    /// Whether an `INSERT` may leave the column out.
    pub has_default: bool,
}

/// One table.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Table {
    /// Columns in declaration order.
    pub columns: Vec<Column>,
    /// The primary key's columns in key order; empty if there is none.
    pub primary_key: Vec<String>,
    /// Other unique keys (each a list of columns in key order), sorted.
    pub unique_keys: Vec<Vec<String>>,
}

/// One applied migration the snapshot was derived from (when it was derived from migrations).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationStamp {
    /// The migration's version.
    pub version: i64,
    /// Its checksum as lower-case hex.
    pub checksum: String,
}

/// A schema snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// [`FORMAT`] when written.
    pub format: u32,
    /// The dialect it was read from (`postgres`, `mysql`, `mariadb`, `sqlite`).
    pub dialect: String,
    /// The migrations it was derived from, ascending; empty when read from a database whose
    /// migrations are not known.
    pub migrations: Vec<MigrationStamp>,
    /// Tables by name.
    pub tables: BTreeMap<String, Table>,
}

/// Why a snapshot could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// The text is not a snapshot.
    Malformed(String),
    /// The file was written by a newer format than this code understands.
    TooNew {
        /// The file's format.
        found: u32,
        /// The newest this code reads.
        supported: u32,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::Malformed(m) => write!(f, "not a schema snapshot: {m}"),
            CatalogError::TooNew { found, supported } => write!(
                f,
                "the snapshot is format {found}, newer than the {supported} this build reads: upgrade persistant"
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

impl Catalog {
    /// The deterministic JSON text of this snapshot (two-space indent, trailing newline): the same
    /// snapshot is always the same bytes, so a committed file diffs cleanly.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("a snapshot always serialises");
        s.push('\n');
        s
    }

    /// Read a snapshot written by [`Catalog::to_json`].
    pub fn from_json(text: &str) -> Result<Catalog, CatalogError> {
        // The format number first, so a newer file is "too new" and not "malformed" even when its
        // other fields have changed shape.
        #[derive(Deserialize)]
        struct Head {
            format: u32,
        }
        let head: Head =
            serde_json::from_str(text).map_err(|e| CatalogError::Malformed(e.to_string()))?;
        if head.format > FORMAT {
            return Err(CatalogError::TooNew {
                found: head.format,
                supported: FORMAT,
            });
        }
        serde_json::from_str(text).map_err(|e| CatalogError::Malformed(e.to_string()))
    }

    /// What differs between `self` (say, the committed file) and `other` (what the database says
    /// now), one line each; empty when they are the same. Migration stamps are compared first, so a
    /// snapshot older than its directory says "stale" and names the migration.
    pub fn differences(&self, other: &Catalog) -> Vec<String> {
        let mut out = Vec::new();
        if self.dialect != other.dialect {
            out.push(format!(
                "dialect: snapshot {}, now {}",
                self.dialect, other.dialect
            ));
        }
        let have: BTreeMap<i64, &str> = self
            .migrations
            .iter()
            .map(|m| (m.version, m.checksum.as_str()))
            .collect();
        for m in &other.migrations {
            match have.get(&m.version) {
                None => out.push(format!(
                    "stale: migration {} is not in the snapshot",
                    m.version
                )),
                Some(c) if *c != m.checksum => out.push(format!(
                    "stale: migration {} changed since the snapshot (checksum differs)",
                    m.version
                )),
                Some(_) => {}
            }
        }
        for m in &self.migrations {
            if !other.migrations.iter().any(|o| o.version == m.version) {
                out.push(format!(
                    "stale: the snapshot has migration {}, which is no longer there",
                    m.version
                ));
            }
        }
        for name in other
            .tables
            .keys()
            .filter(|t| !self.tables.contains_key(*t))
        {
            out.push(format!("table {name}: not in the snapshot"));
        }
        for (name, old) in &self.tables {
            let Some(new) = other.tables.get(name) else {
                out.push(format!("table {name}: in the snapshot, gone now"));
                continue;
            };
            for c in &new.columns {
                match old.columns.iter().find(|o| o.name == c.name) {
                    None => out.push(format!("column {name}.{}: not in the snapshot", c.name)),
                    Some(o) if o != c => out.push(format!(
                        "column {name}.{}: snapshot {} nullable={} default={}, now {} nullable={} default={}",
                        c.name,
                        o.declared_type,
                        o.nullable,
                        o.has_default,
                        c.declared_type,
                        c.nullable,
                        c.has_default
                    )),
                    Some(_) => {}
                }
            }
            for o in old
                .columns
                .iter()
                .filter(|o| !new.columns.iter().any(|c| c.name == o.name))
            {
                out.push(format!(
                    "column {name}.{}: in the snapshot, gone now",
                    o.name
                ));
            }
            if old.primary_key != new.primary_key {
                out.push(format!(
                    "table {name}: primary key snapshot {:?}, now {:?}",
                    old.primary_key, new.primary_key
                ));
            }
            if old.unique_keys != new.unique_keys {
                out.push(format!(
                    "table {name}: unique keys snapshot {:?}, now {:?}",
                    old.unique_keys, new.unique_keys
                ));
            }
        }
        out
    }
}

// ---- reading a database -------------------------------------------------------------------

/// The text of a cell, whatever driver type it came back as.
fn txt(r: &Row, i: usize) -> Result<String, RelError> {
    match r.values.get(i) {
        Some(Value::Text(s)) => Ok(s.clone()),
        Some(Value::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        other => Err(RelError::Backend(format!(
            "catalog column {i} is not text: {other:?}"
        ))),
    }
}

fn opt_txt(r: &Row, i: usize) -> Result<Option<String>, RelError> {
    match r.values.get(i) {
        Some(Value::Null) | None => Ok(None),
        _ => txt(r, i).map(Some),
    }
}

fn flag(r: &Row, i: usize) -> Result<bool, RelError> {
    match r.values.get(i) {
        Some(Value::Int(n)) => Ok(*n != 0),
        Some(Value::Bool(b)) => Ok(*b),
        other => Err(RelError::Backend(format!(
            "catalog column {i} is not a flag: {other:?}"
        ))),
    }
}

/// Stamps from a ledger's `(version, checksum)` rows, ascending, the checksum as lower-case hex.
pub fn stamps(ledger: &[(i64, Vec<u8>)]) -> Vec<MigrationStamp> {
    ledger
        .iter()
        .map(|(version, sum)| MigrationStamp {
            version: *version,
            checksum: sum.iter().map(|b| format!("{b:02x}")).collect(),
        })
        .collect()
}

/// The ledger the migration machinery keeps; never part of a snapshot.
const LEDGER: &str = "_sqlx_migrations";

/// Read the catalog of the connection's current schema (PostgreSQL), database (MySQL, MariaDB) or
/// file (SQLite). `migrations` is stamped into the result as given.
pub async fn read_catalog<B: RelationalBackend>(
    db: &B,
    migrations: Vec<MigrationStamp>,
) -> Result<Catalog, RelError> {
    let dialect = db.dialect();
    let tables = match &dialect {
        Dialect::Postgres => read_postgres(db).await?,
        Dialect::MySql | Dialect::MariaDb => read_mysql(db, &dialect).await?,
        Dialect::Sqlite => read_sqlite(db).await?,
        Dialect::Other(n) => {
            return Err(RelError::Unsupported(format!(
                "no catalog reader for the dialect {n}"
            )));
        }
    };
    let mut tables = tables;
    tables.remove(LEDGER);
    for t in tables.values_mut() {
        t.unique_keys.sort();
        t.unique_keys.dedup();
    }
    Ok(Catalog {
        format: FORMAT,
        dialect: dialect.to_string(),
        migrations,
        tables,
    })
}

async fn read_postgres<B: RelationalBackend>(db: &B) -> Result<BTreeMap<String, Table>, RelError> {
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    let cols = db
        .query(
            "SELECT CAST(c.table_name AS TEXT), CAST(c.column_name AS TEXT), \
             CAST(c.data_type AS TEXT), CAST(c.is_nullable AS TEXT), \
             CASE WHEN c.column_default IS NOT NULL OR c.is_identity = 'YES' THEN 1 ELSE 0 END \
             FROM information_schema.columns c \
             JOIN information_schema.tables t ON t.table_schema = c.table_schema \
             AND t.table_name = c.table_name \
             WHERE c.table_schema = current_schema() AND t.table_type = 'BASE TABLE' \
             ORDER BY c.table_name, c.ordinal_position",
            &[],
        )
        .await?;
    for r in &cols {
        tables.entry(txt(r, 0)?).or_default().columns.push(Column {
            name: txt(r, 1)?,
            declared_type: txt(r, 2)?.to_lowercase(),
            nullable: txt(r, 3)? == "YES",
            has_default: flag(r, 4)?,
        });
    }
    // Unique and primary indexes over plain columns, whole (not partial). One row per index, its
    // columns joined in key order with the unit separator.
    let keys = db
        .query(
            "SELECT CAST(t.relname AS TEXT), CAST(i.relname AS TEXT), \
             CASE WHEN ix.indisprimary THEN 1 ELSE 0 END, \
             CAST(string_agg(CAST(a.attname AS TEXT), chr(31) ORDER BY k.ord) AS TEXT) \
             FROM pg_index ix \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE n.nspname = current_schema() AND ix.indisunique AND ix.indpred IS NULL \
             AND ix.indexprs IS NULL AND t.relkind = 'r' AND k.attnum > 0 \
             GROUP BY t.relname, i.relname, ix.indisprimary \
             ORDER BY t.relname, i.relname",
            &[],
        )
        .await?;
    for r in &keys {
        let cols: Vec<String> = txt(r, 3)?.split('\u{1f}').map(str::to_string).collect();
        if let Some(t) = tables.get_mut(&txt(r, 0)?) {
            if flag(r, 2)? {
                t.primary_key = cols.clone();
                for c in &cols {
                    mark_not_null(t, c);
                }
            } else {
                t.unique_keys.push(cols);
            }
        }
    }
    Ok(tables)
}

fn mark_not_null(t: &mut Table, col: &str) {
    if let Some(c) = t.columns.iter_mut().find(|c| c.name == col) {
        c.nullable = false;
    }
}

async fn read_mysql<B: RelationalBackend>(
    db: &B,
    dialect: &Dialect,
) -> Result<BTreeMap<String, Table>, RelError> {
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    // MariaDB reports a nullable column with no default as the text NULL.
    let default = if *dialect == Dialect::MariaDb {
        "c.COLUMN_DEFAULT IS NOT NULL AND c.COLUMN_DEFAULT <> 'NULL'"
    } else {
        "c.COLUMN_DEFAULT IS NOT NULL"
    };
    let cols = db
        .query(
            &format!(
                "SELECT CAST(c.TABLE_NAME AS CHAR), CAST(c.COLUMN_NAME AS CHAR), \
                 CAST(c.COLUMN_TYPE AS CHAR), CAST(c.IS_NULLABLE AS CHAR), \
                 CASE WHEN {default} OR c.EXTRA LIKE '%auto_increment%' \
                 OR c.EXTRA LIKE '%DEFAULT_GENERATED%' THEN 1 ELSE 0 END \
                 FROM information_schema.COLUMNS c \
                 JOIN information_schema.TABLES t ON t.TABLE_SCHEMA = c.TABLE_SCHEMA \
                 AND t.TABLE_NAME = c.TABLE_NAME \
                 WHERE c.TABLE_SCHEMA = DATABASE() AND t.TABLE_TYPE = 'BASE TABLE' \
                 ORDER BY c.TABLE_NAME, c.ORDINAL_POSITION"
            ),
            &[],
        )
        .await?;
    for r in &cols {
        tables.entry(txt(r, 0)?).or_default().columns.push(Column {
            name: txt(r, 1)?,
            declared_type: txt(r, 2)?.to_lowercase(),
            nullable: txt(r, 3)? == "YES",
            has_default: flag(r, 4)?,
        });
    }
    // Unique indexes (the primary key is the one named PRIMARY), whole columns only: a prefix index
    // (SUB_PART) or a functional one (NULL column name) does not make a conflict target.
    let keys = db
        .query(
            "SELECT CAST(TABLE_NAME AS CHAR), CAST(INDEX_NAME AS CHAR), \
             CAST(COLUMN_NAME AS CHAR), SEQ_IN_INDEX, \
             CASE WHEN SUB_PART IS NOT NULL OR COLUMN_NAME IS NULL THEN 1 ELSE 0 END \
             FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = DATABASE() AND NON_UNIQUE = 0 \
             ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
            &[],
        )
        .await?;
    let mut grouped: BTreeMap<(String, String), (Vec<String>, bool)> = BTreeMap::new();
    for r in &keys {
        let g = grouped.entry((txt(r, 0)?, txt(r, 1)?)).or_default();
        g.0.push(opt_txt(r, 2)?.unwrap_or_default());
        g.1 |= flag(r, 4)?;
    }
    for ((table, index), (cols, partial)) in grouped {
        if partial {
            continue;
        }
        if let Some(t) = tables.get_mut(&table) {
            if index == "PRIMARY" {
                for c in &cols {
                    mark_not_null(t, c);
                }
                t.primary_key = cols;
            } else {
                t.unique_keys.push(cols);
            }
        }
    }
    Ok(tables)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn read_sqlite<B: RelationalBackend>(db: &B) -> Result<BTreeMap<String, Table>, RelError> {
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    let names = db
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            &[],
        )
        .await?;
    for n in &names {
        let name = txt(n, 0)?;
        let mut table = Table::default();
        // cid, name, type, notnull, dflt_value, pk
        let info = db
            .query(&format!("PRAGMA table_info({})", quote_ident(&name)), &[])
            .await?;
        let mut pk: Vec<(i64, String)> = Vec::new();
        let pk_count = info.iter().filter(|r| r.int(5).unwrap_or(0) > 0).count();
        for r in &info {
            let col = txt(r, 1)?;
            let declared = txt(r, 2)?.to_lowercase();
            let notnull = flag(r, 3)?;
            let pk_pos = r.int(5).unwrap_or(0);
            if pk_pos > 0 {
                pk.push((pk_pos, col.clone()));
            }
            // A lone INTEGER PRIMARY KEY is the rowid: it fills itself in when left out.
            let rowid_alias = pk_pos > 0 && pk_count == 1 && declared == "integer";
            table.columns.push(Column {
                name: col,
                declared_type: declared,
                nullable: !notnull && pk_pos == 0,
                has_default: opt_txt(r, 4)?.is_some() || rowid_alias,
            });
        }
        pk.sort();
        table.primary_key = pk.into_iter().map(|(_, c)| c).collect();
        // seq, name, unique, origin, partial
        let indexes = db
            .query(&format!("PRAGMA index_list({})", quote_ident(&name)), &[])
            .await?;
        for ix in &indexes {
            let origin = txt(ix, 3)?;
            if !flag(ix, 2)? || flag(ix, 4)? || origin == "pk" {
                continue;
            }
            // seqno, cid, name (NULL for an expression)
            let cols = db
                .query(
                    &format!("PRAGMA index_info({})", quote_ident(&txt(ix, 1)?)),
                    &[],
                )
                .await?;
            let names: Option<Vec<String>> =
                cols.iter().map(|c| opt_txt(c, 2).ok().flatten()).collect();
            if let Some(names) = names {
                table.unique_keys.push(names);
            }
        }
        tables.insert(name, table);
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, ty: &str, nullable: bool, has_default: bool) -> Column {
        Column {
            name: name.into(),
            declared_type: ty.into(),
            nullable,
            has_default,
        }
    }

    fn sample() -> Catalog {
        let mut tables = BTreeMap::new();
        tables.insert(
            "users".to_string(),
            Table {
                columns: vec![
                    col("id", "bigint", false, true),
                    col("email", "text", false, false),
                    col("note", "text", true, false),
                ],
                primary_key: vec!["id".into()],
                unique_keys: vec![vec!["email".into()]],
            },
        );
        Catalog {
            format: FORMAT,
            dialect: "postgres".into(),
            migrations: vec![MigrationStamp {
                version: 1,
                checksum: "ab12".into(),
            }],
            tables,
        }
    }

    #[test]
    fn json_round_trips_and_is_deterministic() {
        let c = sample();
        let text = c.to_json();
        assert_eq!(Catalog::from_json(&text).unwrap(), c);
        assert_eq!(c.to_json(), text, "the same snapshot is the same bytes");
        assert!(text.ends_with("}\n"));
        assert!(text.contains("\"type\": \"bigint\""), "{text}");
    }

    #[test]
    fn a_newer_format_is_refused_not_guessed() {
        let text = sample().to_json().replace("\"format\": 1", "\"format\": 2");
        assert_eq!(
            Catalog::from_json(&text),
            Err(CatalogError::TooNew {
                found: 2,
                supported: FORMAT
            })
        );
        // A newer file whose other fields changed shape is still "too new", not "malformed".
        assert!(matches!(
            Catalog::from_json(r#"{"format": 9, "tables": 5}"#),
            Err(CatalogError::TooNew { found: 9, .. })
        ));
        // The control: the same text at the supported format reads.
        assert!(Catalog::from_json(&sample().to_json()).is_ok());
        assert!(matches!(
            Catalog::from_json("[]"),
            Err(CatalogError::Malformed(_))
        ));
    }

    #[test]
    fn identical_snapshots_differ_in_nothing() {
        assert!(sample().differences(&sample()).is_empty());
    }

    #[test]
    fn each_kind_of_change_is_named() {
        let old = sample();
        let mut now = sample();
        now.migrations.push(MigrationStamp {
            version: 2,
            checksum: "cd34".into(),
        });
        let t = now.tables.get_mut("users").unwrap();
        t.columns[2].nullable = false;
        t.columns.push(col("age", "integer", true, false));
        t.columns.remove(1);
        t.unique_keys.clear();
        now.tables.insert("orders".into(), Table::default());
        let d = old.differences(&now).join("\n");
        for want in [
            "stale: migration 2 is not in the snapshot",
            "table orders: not in the snapshot",
            "column users.age: not in the snapshot",
            "column users.email: in the snapshot, gone now",
            "column users.note: snapshot text nullable=true default=false, now text nullable=false default=false",
            "unique keys snapshot",
        ] {
            assert!(d.contains(want), "missing {want:?} in:\n{d}");
        }
        // A changed checksum is stale too.
        let mut changed = sample();
        changed.migrations[0].checksum = "ff".into();
        assert!(old.differences(&changed)[0].contains("changed since the snapshot"));
    }
}
