// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;

/// The SQL dialect a backend speaks, which decides how a statement is written.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Dialect {
    /// PostgreSQL (and wire-compatible servers that keep its SQL).
    Postgres,
    /// SQLite.
    Sqlite,
    /// MySQL.
    MySql,
    /// MariaDB.
    MariaDb,
    /// Anything else, by name.
    Other(String),
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dialect::Postgres => f.write_str("postgres"),
            Dialect::Sqlite => f.write_str("sqlite"),
            Dialect::MySql => f.write_str("mysql"),
            Dialect::MariaDb => f.write_str("mariadb"),
            Dialect::Other(n) => f.write_str(n),
        }
    }
}

/// A portable parameter or column value. Deliberately narrow: what a key-value or migration
/// statement needs. A program with richer types uses the driver's own handle.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit integer (also read from narrower integer columns).
    Int(i64),
    /// 64-bit float (also read from single-precision columns).
    Float(f64),
    /// Text.
    Text(String),
    /// Bytes (`BYTEA`, `BLOB`).
    Bytes(Vec<u8>),
}

/// One result row, columns in select order.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// The column values.
    pub values: Vec<Value>,
}

impl Row {
    /// A row of `values`.
    pub fn new(values: Vec<Value>) -> Self {
        Row { values }
    }

    /// Column `i` as bytes, if it is `Bytes`.
    pub fn bytes(&self, i: usize) -> Option<&[u8]> {
        match self.values.get(i) {
            Some(Value::Bytes(b)) => Some(b),
            _ => None,
        }
    }

    /// Column `i` as an integer, if it is `Int`.
    pub fn int(&self, i: usize) -> Option<i64> {
        match self.values.get(i) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        }
    }

    /// Column `i` as text, if it is `Text`.
    pub fn text(&self, i: usize) -> Option<&str> {
        match self.values.get(i) {
            Some(Value::Text(s)) => Some(s),
            _ => None,
        }
    }
}
