// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed rows: a struct read from a result row, and the compile-time checks `sql!(as T, ..)` makes
//! between a `SELECT` list and that struct. Feature `sql`.
//!
//! A row is read **by position**: field `n` takes column `n`. The names are checked at build time
//! (and the types, and nullability, against the schema snapshot), not used at run time.
//!
//! The types are the intersection the engines share, and each is a documented loss against the
//! engine's own (`docs/DECODER-AUDIT.md`): `i64` and `i32` (an integer column of any width), `f64`,
//! `bool` (a boolean, or an integer 0 or 1: MySQL has no other), `String`, `Vec<u8>`, and `Option` of
//! any of them for a column that can be NULL. Anything finer (UUID, decimal, timestamps) is read with
//! the driver's own types, through `PgBackend::pool()` and friends.

use crate::{RelError, Row, Value};
use std::marker::PhantomData;
use unisql_embed::Query;

/// What a field holds, as far as a column's class is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FieldKind {
    /// `i64`, `i32`.
    Int,
    /// `f64`.
    Float,
    /// `bool`.
    Bool,
    /// `String`.
    Text,
    /// `Vec<u8>`.
    Bytes,
}

impl FieldKind {
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// One field of a [`FromRow`] struct, as `#[derive(FromRow)]` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The field's name (what `sql!(as T, ..)` matches against the column's name).
    pub name: &'static str,
    /// What it holds.
    pub kind: FieldKind,
    /// Whether it is an `Option`, so a NULL column can be read into it.
    pub nullable: bool,
}

/// A type that can be read from a result row. Derive it: `#[derive(persistant::FromRow)]`.
pub trait FromRow: Sized {
    /// The fields in order; the checks of `sql!(as T, ..)` compare the `SELECT` list with these.
    const FIELDS: &'static [Field];

    /// Read the row, field `n` from column `n`.
    fn from_row(row: &Row) -> Result<Self, RelError>;
}

/// A value a field can hold. Implemented for the types listed in the module documentation; the derive
/// needs nothing else from a field's type, so an unsupported type is the compiler's "trait bound not
/// satisfied", at the field.
pub trait FromValue: Sized {
    /// What the type holds.
    const KIND: FieldKind;
    /// Whether the type is an `Option`.
    const NULLABLE: bool = false;

    /// Read the type from a cell; `Err` is the reason it cannot.
    fn from_value(v: &Value) -> Result<Self, String>;
}

fn wrong(expected: &str, got: &Value) -> String {
    format!("expected {expected}, got {got:?}")
}

impl FromValue for i64 {
    const KIND: FieldKind = FieldKind::Int;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Int(n) => Ok(*n),
            other => Err(wrong("an integer", other)),
        }
    }
}

impl FromValue for i32 {
    const KIND: FieldKind = FieldKind::Int;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Int(n) => i32::try_from(*n).map_err(|_| format!("{n} does not fit an i32")),
            other => Err(wrong("an integer", other)),
        }
    }
}

impl FromValue for f64 {
    const KIND: FieldKind = FieldKind::Float;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Float(f) => Ok(*f),
            other => Err(wrong("a float", other)),
        }
    }
}

impl FromValue for bool {
    const KIND: FieldKind = FieldKind::Bool;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Bool(b) => Ok(*b),
            // MySQL and SQLite store a boolean as an integer.
            Value::Int(0) => Ok(false),
            Value::Int(1) => Ok(true),
            other => Err(wrong("a boolean (or 0 or 1)", other)),
        }
    }
}

impl FromValue for String {
    const KIND: FieldKind = FieldKind::Text;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Text(s) => Ok(s.clone()),
            // MySQL returns some text columns as bytes.
            Value::Bytes(b) => {
                String::from_utf8(b.clone()).map_err(|_| "bytes that are not UTF-8".to_string())
            }
            other => Err(wrong("text", other)),
        }
    }
}

impl FromValue for Vec<u8> {
    const KIND: FieldKind = FieldKind::Bytes;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Bytes(b) => Ok(b.clone()),
            other => Err(wrong("bytes", other)),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    const KIND: FieldKind = T::KIND;
    const NULLABLE: bool = true;
    fn from_value(v: &Value) -> Result<Self, String> {
        match v {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

/// Read field `index` (named `name`) of `T` from `row`. Used by `#[derive(FromRow)]`.
pub fn read<T: FromValue>(row: &Row, index: usize, name: &str) -> Result<T, RelError> {
    let v = row.values.get(index).ok_or_else(|| {
        RelError::Backend(format!(
            "the row has {} columns, but field `{name}` is column {}",
            row.values.len(),
            index + 1
        ))
    })?;
    T::from_value(v)
        .map_err(|why| RelError::Backend(format!("column {} (field `{name}`): {why}", index + 1)))
}

/// A statement built by `sql!(as T, ..)`: it was checked at build time to select what `T` reads.
/// Run it with `query_as`, which only accepts a matching `T`.
#[derive(Debug)]
pub struct TypedSql<T> {
    /// The statement.
    pub query: Query,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for TypedSql<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for TypedSql<T> {}

impl<T> TypedSql<T> {
    /// Wrap a checked statement. Built by the macro.
    pub const fn new(query: Query) -> Self {
        TypedSql {
            query,
            marker: PhantomData,
        }
    }
}

/// The compile-time checks `sql!(as T, ..)` makes, as `const fn`s so that a mismatch is a build
/// error (the message names the column). Not for direct use.
#[doc(hidden)]
pub mod check {
    use super::{FieldKind, FromRow};

    /// `u8` masks of acceptable [`FieldKind`]s, by the column's class.
    pub const NUMERIC: u8 = FieldKind::Int.bit() | FieldKind::Float.bit();
    /// A text column.
    pub const TEXT: u8 = FieldKind::Text.bit();
    /// A boolean column.
    pub const BOOLEAN: u8 = FieldKind::Bool.bit();
    /// A column of another class (dates, UUIDs, JSON, bytes): bytes or text.
    pub const OTHER: u8 = FieldKind::Bytes.bit() | FieldKind::Text.bit();
    /// A column whose class is not known: anything.
    pub const ANY: u8 = 0xff;

    const fn str_eq(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    /// The struct has `n` fields.
    pub const fn fields_are<T: FromRow>(n: usize) -> bool {
        T::FIELDS.len() == n
    }

    /// Field `index` is named `name`.
    pub const fn named<T: FromRow>(index: usize, name: &str) -> bool {
        index < T::FIELDS.len() && str_eq(T::FIELDS[index].name, name)
    }

    /// Field `index` holds a kind in `mask`.
    pub const fn kind_in<T: FromRow>(index: usize, mask: u8) -> bool {
        index < T::FIELDS.len() && T::FIELDS[index].kind.bit() & mask != 0
    }

    /// If the column can be NULL (`column_nullable` is true), field `index` is an `Option`.
    pub const fn null_fits<T: FromRow>(index: usize, column_nullable: bool) -> bool {
        index < T::FIELDS.len() && (!column_nullable || T::FIELDS[index].nullable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct User {
        id: i64,
        email: String,
        note: Option<String>,
    }

    impl FromRow for User {
        const FIELDS: &'static [Field] = &[
            Field {
                name: "id",
                kind: <i64 as FromValue>::KIND,
                nullable: <i64 as FromValue>::NULLABLE,
            },
            Field {
                name: "email",
                kind: <String as FromValue>::KIND,
                nullable: <String as FromValue>::NULLABLE,
            },
            Field {
                name: "note",
                kind: <Option<String> as FromValue>::KIND,
                nullable: <Option<String> as FromValue>::NULLABLE,
            },
        ];
        fn from_row(row: &Row) -> Result<Self, RelError> {
            Ok(User {
                id: read(row, 0, "id")?,
                email: read(row, 1, "email")?,
                note: read(row, 2, "note")?,
            })
        }
    }

    #[test]
    fn a_row_is_read_by_position_with_each_documented_loss() {
        let row = Row::new(vec![Value::Int(7), Value::Text("a@x".into()), Value::Null]);
        let u = User::from_row(&row).unwrap();
        assert_eq!((u.id, u.email.as_str(), u.note), (7, "a@x", None));
        // MySQL returns some text as bytes; a boolean is 0 or 1 there.
        assert_eq!(
            String::from_value(&Value::Bytes(b"hi".to_vec())).unwrap(),
            "hi"
        );
        assert!(bool::from_value(&Value::Int(1)).unwrap());
        assert!(!bool::from_value(&Value::Int(0)).unwrap());
        assert_eq!(Option::<i64>::from_value(&Value::Null).unwrap(), None);
        assert_eq!(Option::<i64>::from_value(&Value::Int(3)).unwrap(), Some(3));
    }

    #[test]
    fn what_cannot_be_read_says_which_column_and_why() {
        let bad = Row::new(vec![
            Value::Text("x".into()),
            Value::Text("a".into()),
            Value::Null,
        ]);
        let e = User::from_row(&bad).unwrap_err().to_string();
        assert!(
            e.contains("column 1 (field `id`): expected an integer"),
            "{e}"
        );
        let short = Row::new(vec![Value::Int(1)]);
        let e = User::from_row(&short).unwrap_err().to_string();
        assert!(
            e.contains("the row has 1 columns, but field `email` is column 2"),
            "{e}"
        );
        assert!(i32::from_value(&Value::Int(i64::MAX)).is_err());
        assert!(bool::from_value(&Value::Int(2)).is_err());
        // NULL in a field that is not an Option is an error, not a default.
        assert!(i64::from_value(&Value::Null).is_err());
    }

    #[test]
    fn the_const_checks_compare_name_kind_and_nullability() {
        use check::*;
        assert!(fields_are::<User>(3) && !fields_are::<User>(2));
        assert!(named::<User>(1, "email") && !named::<User>(1, "mail") && !named::<User>(9, "x"));
        assert!(kind_in::<User>(0, NUMERIC) && !kind_in::<User>(0, TEXT));
        assert!(kind_in::<User>(1, TEXT) && kind_in::<User>(1, OTHER) && kind_in::<User>(1, ANY));
        // A nullable column needs an Option field; a NOT NULL column accepts either.
        assert!(!null_fits::<User>(1, true) && null_fits::<User>(2, true));
        assert!(null_fits::<User>(1, false) && null_fits::<User>(2, false));
        // Evaluated at compile time too: a wrong answer here fails the build.
        const _: () = assert!(named::<User>(0, "id") && fields_are::<User>(3));
    }
}
