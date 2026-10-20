// SPDX-License-Identifier: MIT OR Apache-2.0
//! A schema snapshot: what a database's catalog says about its tables, read through the
//! [`RelationalBackend`] trait on PostgreSQL, MySQL/MariaDB and SQLite, as UniSQL's [`Snapshot`] (the
//! file format is UniSQL's; this repo writes it, `docs/COMPILE-TIME-SQL.md` sections 1 and 7).
//! Feature `catalog`.
//!
//! The file records structure only, never data. Tables are the base tables of the connection's
//! current schema or database; the migration ledger `_sqlx_migrations` is never in it. The snapshot's
//! `meta` carries the dialect and, when it was derived from migrations, each one's version and
//! checksum, so a snapshot older than its directory is *detected* ([`differences`]).
//!
//! What each engine can tell us, and how it is normalised:
//! * `declared_type` is the engine's own spelling, lower-cased (`bigint`, `character varying`,
//!   `varchar(40)`, `integer`). It is classified (`Text`, `Numeric`, `Boolean`, `Other`, `Unknown`)
//!   from that spelling by the engine's own rules; a type no rule recognises is `Unknown`, never
//!   guessed. No collation is recorded (an unstated collation is unknown, not the default).
//! * `nullable` is the column's `NOT NULL` flag inverted; a primary-key column is not nullable on every
//!   engine, which is the declared intent (only an `INTEGER PRIMARY KEY` is truly never NULL in SQLite).
//! * `has_default` means "the column may be left out of an `INSERT`": a `DEFAULT`, an identity or
//!   auto-increment column, or SQLite's rowid alias. A MariaDB `COLUMN_DEFAULT` of the text `NULL`
//!   (how it reports a nullable column with no default) is not a default.
//! * Keys are the primary key and the unique constraints or unique indexes over plain columns, in key
//!   order. A table with a partial or expression unique index has its key list marked **unreliable**
//!   (UniSQL then derives no conflict key from it) rather than that index being silently dropped.
//! * Every table's column list is marked complete: it was read in full.

use crate::{Dialect, RelError, RelationalBackend, Row, Value};
use std::collections::BTreeMap;
pub use unisql_core::snapshot::{Snapshot, SnapshotError};
pub use unisql_core::{Column, ColumnClass, Schema};

/// The ledger the migration machinery keeps; never part of a snapshot.
const LEDGER: &str = "_sqlx_migrations";

/// Read a snapshot written by [`Snapshot::to_json`]; a file of a newer format is refused ("update
/// unisql"), not guessed at.
pub fn parse(text: &str) -> Result<Snapshot, SnapshotError> {
    Snapshot::from_json(text)
}

/// The `meta` entries that record which migrations a snapshot was derived from: one per
/// `(version, checksum)` row of the ledger, the key sorted by version, the checksum as lower-case hex.
pub fn migration_meta(ledger: &[(i64, Vec<u8>)]) -> BTreeMap<String, String> {
    ledger
        .iter()
        .map(|(version, sum)| {
            (
                format!("migration.{version:020}"),
                sum.iter().map(|b| format!("{b:02x}")).collect(),
            )
        })
        .collect()
}

fn migrations_of(s: &Snapshot) -> BTreeMap<i64, &str> {
    s.meta
        .iter()
        .filter_map(|(k, v)| {
            let version = k.strip_prefix("migration.")?.parse::<i64>().ok()?;
            Some((version, v.as_str()))
        })
        .collect()
}

type TableFacts = (BTreeMap<String, Column>, Vec<Vec<String>>, bool);

fn tables_of(s: &Snapshot) -> BTreeMap<String, TableFacts> {
    s.schema
        .tables()
        .map(|t| {
            (
                t.name.join("."),
                (t.columns.clone(), t.unique_keys.to_vec(), t.keys_unreliable),
            )
        })
        .collect()
}

/// What differs between `old` (say, the committed file) and `new` (what the database says now), one
/// line each; empty when they are the same. The migration stamps are compared first, so a snapshot
/// older than its directory says "stale" and names the migration.
pub fn differences(old: &Snapshot, new: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    let (od, nd) = (old.meta.get("dialect"), new.meta.get("dialect"));
    if od != nd {
        out.push(format!(
            "dialect: snapshot {}, now {}",
            od.map_or("?", String::as_str),
            nd.map_or("?", String::as_str)
        ));
    }
    let (have, want) = (migrations_of(old), migrations_of(new));
    for (v, sum) in &want {
        match have.get(v) {
            None => out.push(format!("stale: migration {v} is not in the snapshot")),
            Some(c) if c != sum => out.push(format!(
                "stale: migration {v} changed since the snapshot (checksum differs)"
            )),
            Some(_) => {}
        }
    }
    for v in have.keys().filter(|v| !want.contains_key(*v)) {
        out.push(format!(
            "stale: the snapshot has migration {v}, which is no longer there"
        ));
    }
    let (ot, nt) = (tables_of(old), tables_of(new));
    for name in nt.keys().filter(|t| !ot.contains_key(*t)) {
        out.push(format!("table {name}: not in the snapshot"));
    }
    for (name, (ocols, okeys, ounrel)) in &ot {
        let Some((ncols, nkeys, nunrel)) = nt.get(name) else {
            out.push(format!("table {name}: in the snapshot, gone now"));
            continue;
        };
        for (c, col) in ncols {
            match ocols.get(c) {
                None => out.push(format!("column {name}.{c}: not in the snapshot")),
                Some(o) if o != col => out.push(format!(
                    "column {name}.{c}: snapshot {} {:?} nullable={:?} default={:?}, now {} {:?} nullable={:?} default={:?}",
                    o.declared_type.as_deref().unwrap_or("?"),
                    o.class,
                    o.nullable,
                    o.has_default,
                    col.declared_type.as_deref().unwrap_or("?"),
                    col.class,
                    col.nullable,
                    col.has_default
                )),
                Some(_) => {}
            }
        }
        for c in ocols.keys().filter(|c| !ncols.contains_key(*c)) {
            out.push(format!("column {name}.{c}: in the snapshot, gone now"));
        }
        if okeys != nkeys {
            out.push(format!(
                "table {name}: unique keys snapshot {okeys:?}, now {nkeys:?}"
            ));
        }
        if ounrel != nunrel {
            out.push(format!(
                "table {name}: key list reliable={}, was {}",
                !nunrel, !ounrel
            ));
        }
    }
    out
}

// ---- classifying a declared type -----------------------------------------------------------

/// The comparison class of a declared type, by each engine's own naming. A type none of the rules
/// recognises is [`ColumnClass::Unknown`].
pub fn classify(dialect: &Dialect, declared: &str) -> ColumnClass {
    let t = declared.trim().to_lowercase();
    let base = t.split(['(', ' ']).next().unwrap_or("");
    match dialect {
        Dialect::Postgres => match t.as_str() {
            "text" | "character varying" | "character" | "name" | "citext" | "bpchar" => {
                ColumnClass::Text
            }
            "smallint" | "integer" | "bigint" | "numeric" | "decimal" | "real"
            | "double precision" | "money" => ColumnClass::Numeric,
            "boolean" => ColumnClass::Boolean,
            "bytea" | "uuid" | "json" | "jsonb" | "date" | "interval" | "inet" | "cidr"
            | "macaddr" | "xml" | "array" | "bit" | "bit varying" | "oid" => ColumnClass::Other,
            t if t.starts_with("timestamp") || t.starts_with("time ") || t == "time" => {
                ColumnClass::Other
            }
            _ => ColumnClass::Unknown,
        },
        Dialect::MySql | Dialect::MariaDb => match base {
            "char" | "varchar" | "tinytext" | "text" | "mediumtext" | "longtext" | "enum"
            | "set" => ColumnClass::Text,
            // `tinyint(1)` is how MySQL spells a boolean, but it is an integer there: not Boolean.
            "tinyint" | "smallint" | "mediumint" | "int" | "integer" | "bigint" | "decimal"
            | "numeric" | "float" | "double" | "real" | "year" => ColumnClass::Numeric,
            "binary" | "varbinary" | "tinyblob" | "blob" | "mediumblob" | "longblob" | "date"
            | "datetime" | "timestamp" | "time" | "json" | "bit" | "uuid" | "inet4" | "inet6"
            | "geometry" | "point" | "linestring" | "polygon" => ColumnClass::Other,
            _ => ColumnClass::Unknown,
        },
        Dialect::Sqlite => {
            // SQLite's own affinity rules (https://www.sqlite.org/datatype3.html section 3.1), in
            // their order. A declared `boolean` is numeric affinity but says what it means.
            if t == "boolean" || t == "bool" {
                ColumnClass::Boolean
            } else if t.contains("int") {
                ColumnClass::Numeric
            } else if t.contains("char") || t.contains("clob") || t.contains("text") {
                ColumnClass::Text
            } else if t.is_empty() || t.contains("blob") {
                ColumnClass::Other
            } else if t.contains("real")
                || t.contains("floa")
                || t.contains("doub")
                || t.contains("numeric")
                || t.contains("decimal")
            {
                ColumnClass::Numeric
            } else if t.contains("date") || t.contains("time") || t.contains("json") {
                ColumnClass::Other
            } else {
                ColumnClass::Unknown
            }
        }
        Dialect::Other(_) => ColumnClass::Unknown,
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

/// One table as read, before it becomes part of the [`Schema`].
#[derive(Default)]
struct Read {
    /// name, declared type (lower case), nullable, has default
    columns: Vec<(String, String, bool, bool)>,
    primary_key: Vec<String>,
    unique_keys: Vec<Vec<String>>,
    /// A unique index the reader could not express as a list of plain columns exists.
    keys_unreliable: bool,
}

impl Read {
    fn mark_not_null(&mut self, col: &str) {
        if let Some(c) = self.columns.iter_mut().find(|c| c.0 == col) {
            c.2 = false;
        }
    }
}

/// Read the catalog of the connection's current schema (PostgreSQL), database (MySQL, MariaDB) or
/// file (SQLite). `meta` is stored in the snapshot as given, with the dialect added.
pub async fn read_catalog<B: RelationalBackend>(
    db: &B,
    mut meta: BTreeMap<String, String>,
) -> Result<Snapshot, RelError> {
    let dialect = db.dialect();
    let mut tables = match &dialect {
        Dialect::Postgres => read_postgres(db).await?,
        Dialect::MySql | Dialect::MariaDb => read_mysql(db, &dialect).await?,
        Dialect::Sqlite => read_sqlite(db).await?,
        Dialect::Other(n) => {
            return Err(RelError::Unsupported(format!(
                "no catalog reader for the dialect {n}"
            )));
        }
    };
    tables.remove(LEDGER);
    let mut schema = Schema::new();
    for (name, mut t) in tables {
        let table = vec![name];
        for (col, declared, nullable, has_default) in &t.columns {
            schema.set_column(
                table.clone(),
                col.clone(),
                Column::new(classify(&dialect, declared))
                    .with_declared_type(declared.clone())
                    .with_nullable(*nullable)
                    .with_default(*has_default),
            );
        }
        schema.mark_columns_complete(table.clone());
        t.unique_keys.sort();
        t.unique_keys.dedup();
        if !t.primary_key.is_empty() {
            schema.add_unique_key(table.clone(), t.primary_key.clone());
        }
        for k in t.unique_keys {
            if k != t.primary_key {
                schema.add_unique_key(table.clone(), k);
            }
        }
        if t.keys_unreliable {
            schema.mark_keys_unreliable(table);
        }
    }
    meta.insert("dialect".to_string(), dialect.to_string());
    let mut snapshot = Snapshot::new(schema);
    snapshot.meta = meta;
    Ok(snapshot)
}

async fn read_postgres<B: RelationalBackend>(db: &B) -> Result<BTreeMap<String, Read>, RelError> {
    let mut tables: BTreeMap<String, Read> = BTreeMap::new();
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
        tables.entry(txt(r, 0)?).or_default().columns.push((
            txt(r, 1)?,
            txt(r, 2)?.to_lowercase(),
            txt(r, 3)? == "YES",
            flag(r, 4)?,
        ));
    }
    // Every unique index, with its plain columns joined in key order by the unit separator, and
    // whether it is partial or over an expression (then its column list is not the whole story).
    let keys = db
        .query(
            "SELECT CAST(t.relname AS TEXT), CAST(i.relname AS TEXT), \
             CASE WHEN ix.indisprimary THEN 1 ELSE 0 END, \
             CASE WHEN ix.indpred IS NOT NULL OR ix.indexprs IS NOT NULL THEN 1 ELSE 0 END, \
             CAST(string_agg(CAST(a.attname AS TEXT), chr(31) ORDER BY k.ord) AS TEXT) \
             FROM pg_index ix \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, ord) \
             LEFT JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             AND k.attnum > 0 \
             WHERE n.nspname = current_schema() AND ix.indisunique AND t.relkind = 'r' \
             GROUP BY t.relname, i.relname, ix.indisprimary, ix.indpred, ix.indexprs \
             ORDER BY t.relname, i.relname",
            &[],
        )
        .await?;
    for r in &keys {
        let Some(t) = tables.get_mut(&txt(r, 0)?) else {
            continue;
        };
        if flag(r, 3)? {
            t.keys_unreliable = true;
            continue;
        }
        let cols: Vec<String> = txt(r, 4)?.split('\u{1f}').map(str::to_string).collect();
        if flag(r, 2)? {
            for c in &cols {
                t.mark_not_null(c);
            }
            t.primary_key = cols;
        } else {
            t.unique_keys.push(cols);
        }
    }
    Ok(tables)
}

async fn read_mysql<B: RelationalBackend>(
    db: &B,
    dialect: &Dialect,
) -> Result<BTreeMap<String, Read>, RelError> {
    let mut tables: BTreeMap<String, Read> = BTreeMap::new();
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
        tables.entry(txt(r, 0)?).or_default().columns.push((
            txt(r, 1)?,
            txt(r, 2)?.to_lowercase(),
            txt(r, 3)? == "YES",
            flag(r, 4)?,
        ));
    }
    // Unique indexes (the primary key is the one named PRIMARY). A prefix index (SUB_PART) or a
    // functional one (NULL column name) does not make a conflict target and marks the list unreliable.
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
        let Some(t) = tables.get_mut(&table) else {
            continue;
        };
        if partial {
            t.keys_unreliable = true;
        } else if index == "PRIMARY" {
            for c in &cols {
                t.mark_not_null(c);
            }
            t.primary_key = cols;
        } else {
            t.unique_keys.push(cols);
        }
    }
    Ok(tables)
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

async fn read_sqlite<B: RelationalBackend>(db: &B) -> Result<BTreeMap<String, Read>, RelError> {
    let mut tables: BTreeMap<String, Read> = BTreeMap::new();
    let names = db
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
            &[],
        )
        .await?;
    for n in &names {
        let name = txt(n, 0)?;
        let mut table = Read::default();
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
            table.columns.push((
                col,
                declared,
                !notnull && pk_pos == 0,
                opt_txt(r, 4)?.is_some() || rowid_alias,
            ));
        }
        pk.sort();
        table.primary_key = pk.into_iter().map(|(_, c)| c).collect();
        // seq, name, unique, origin, partial
        let indexes = db
            .query(&format!("PRAGMA index_list({})", quote_ident(&name)), &[])
            .await?;
        for ix in &indexes {
            if !flag(ix, 2)? || txt(ix, 3)? == "pk" {
                continue;
            }
            if flag(ix, 4)? {
                table.keys_unreliable = true;
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
            match names {
                Some(names) => table.unique_keys.push(names),
                None => table.keys_unreliable = true,
            }
        }
        tables.insert(name, table);
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(dialect: &str, migrations: &[(i64, &str)], cols: &[(&str, ColumnClass)]) -> Snapshot {
        let mut schema = Schema::new();
        for (name, class) in cols {
            schema.set_column(
                vec!["users".to_string()],
                name.to_string(),
                Column::new(*class).with_nullable(false).with_default(false),
            );
        }
        schema.mark_columns_complete(vec!["users".to_string()]);
        schema.add_unique_key(vec!["users".to_string()], vec!["id".to_string()]);
        let mut s = Snapshot::new(schema);
        s.meta.insert("dialect".into(), dialect.into());
        for (v, c) in migrations {
            s.meta.insert(format!("migration.{v:020}"), (*c).into());
        }
        s
    }

    const COLS: [(&str, ColumnClass); 2] =
        [("id", ColumnClass::Numeric), ("email", ColumnClass::Text)];

    #[test]
    fn the_file_round_trips_is_deterministic_and_refuses_a_newer_format() {
        let s = snap("postgres", &[(1, "ab12")], &COLS);
        let text = s.to_json();
        assert_eq!(parse(&text).unwrap(), s);
        assert_eq!(s.to_json(), text, "the same snapshot is the same bytes");
        let newer = text.replace("\"format\": 1", "\"format\": 9");
        assert!(matches!(
            parse(&newer),
            Err(SnapshotError::TooNew { found: 9, .. })
        ));
        // The control: the same text at the supported format reads.
        assert!(parse(&text).is_ok());
    }

    #[test]
    fn migration_meta_sorts_by_version_not_by_text() {
        let m = migration_meta(&[(2, vec![0xab]), (10, vec![0x01, 0xff])]);
        let keys: Vec<_> = m.keys().cloned().collect();
        assert_eq!(
            keys,
            [
                "migration.00000000000000000002",
                "migration.00000000000000000010"
            ]
        );
        assert_eq!(m["migration.00000000000000000010"], "01ff");
    }

    #[test]
    fn identical_snapshots_differ_in_nothing() {
        let a = snap("postgres", &[(1, "ab12")], &COLS);
        assert!(differences(&a, &a.clone()).is_empty());
    }

    #[test]
    fn each_kind_of_change_is_named() {
        let old = snap("postgres", &[(1, "ab12")], &COLS);
        let mut now = snap(
            "postgres",
            &[(1, "ab12"), (2, "cd34")],
            &[("id", ColumnClass::Numeric), ("age", ColumnClass::Numeric)],
        );
        now.schema.mark_keys_unreliable(vec!["users".to_string()]);
        now.schema.set_column(
            vec!["orders".into()],
            "id".into(),
            Column::new(ColumnClass::Numeric),
        );
        let d = differences(&old, &now).join("\n");
        for want in [
            "stale: migration 2 is not in the snapshot",
            "table orders: not in the snapshot",
            "column users.age: not in the snapshot",
            "column users.email: in the snapshot, gone now",
            "table users: key list reliable=false, was true",
        ] {
            assert!(d.contains(want), "missing {want:?} in:\n{d}");
        }
        // A changed checksum is stale too; a vanished migration is named; a dialect change is named.
        let changed = snap("mysql", &[(1, "ff")], &COLS);
        let d = differences(&old, &changed);
        assert!(
            d.iter().any(|l| l.contains("changed since the snapshot")),
            "{d:?}"
        );
        assert!(d.iter().any(|l| l.starts_with("dialect:")), "{d:?}");
        let gone = snap("postgres", &[], &COLS);
        assert!(
            differences(&old, &gone)
                .iter()
                .any(|l| l.contains("migration 1, which is no longer there"))
        );
    }

    #[test]
    fn declared_types_are_classified_by_each_engines_own_rules() {
        use ColumnClass::*;
        let pg = Dialect::Postgres;
        for (t, want) in [
            ("bigint", Numeric),
            ("character varying", Text),
            ("boolean", Boolean),
            ("bytea", Other),
            ("timestamp with time zone", Other),
            ("citext_domain", Unknown),
        ] {
            assert_eq!(classify(&pg, t), want, "postgres {t}");
        }
        let my = Dialect::MySql;
        for (t, want) in [
            ("bigint", Numeric),
            ("varchar(40)", Text),
            ("tinyint(1)", Numeric), // MySQL's boolean is an integer: not Boolean
            ("longblob", Other),
            ("datetime(6)", Other),
            ("geomcollection", Unknown),
        ] {
            assert_eq!(classify(&my, t), want, "mysql {t}");
        }
        let lite = Dialect::Sqlite;
        for (t, want) in [
            ("integer", Numeric),
            ("bigint", Numeric), // contains "int"
            ("varchar(80)", Text),
            ("text", Text),
            ("blob", Other),
            ("", Other),
            ("double precision", Numeric),
            ("boolean", Boolean),
            ("frobnicate", Unknown),
        ] {
            assert_eq!(classify(&lite, t), want, "sqlite {t}");
        }
        assert_eq!(classify(&Dialect::Other("x".into()), "int"), Unknown);
    }
}
