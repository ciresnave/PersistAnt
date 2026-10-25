// SPDX-License-Identifier: MIT OR Apache-2.0
//! The compile-time SQL macro behind `persistant::sql!` (feature `sql-macros` of the `persistant`
//! crate). A proc-macro crate cannot be extended, only wrapped, so this is PersistAnt's own thin
//! wrapper over `unisql-macro-core`; see `docs/COMPILE-TIME-SQL.md`.
//!
//! ```text
//! const FIND: persistant::Sql = persistant::sql!(
//!     source = postgres, targets = [postgres, sqlite, mysql],
//!     "SELECT id, email FROM users WHERE id = $1"
//! );
//! ```
//!
//! Arguments, in any order, then the statement as the last argument (a string literal):
//! `source` (required), `targets` (required, at least one), `allow`, `requires`, `forbids`, `schema`,
//! `params = (T1, T2, ..)` (the Rust type of each `$n`, checked against the plain column it meets in a
//! `WHERE`, `INSERT .. VALUES` or `SET`; see `docs/COMPILE-TIME-SQL.md` 7b), and
//! `crate = path` (where the expansion finds `Query`, default `::persistant::embed`). The statement is
//! parsed in the source dialect and translated for every named target at build time; whatever cannot
//! be is a compile error. Nothing is looked up in a database; the catalog checks read the committed
//! schema snapshot.

mod checker;
mod imp;
mod rows;
mod sqlx_bridge;
mod sqlx_offline;

/// Check and translate a literal statement at build time. See the crate documentation.
#[proc_macro]
pub fn sql(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    imp::sql(input.into()).into()
}

/// Derive `FromRow` for a struct with named fields, each of a type the crate can read from a cell
/// (`i64`, `i32`, `f64`, `bool`, `String`, `Vec<u8>`, or `Option` of those). The struct is then read from
/// a result row by position, and `sql!(as T, ..)` can check a `SELECT` against it at build time.
/// `#[persistant(crate = path)]` names the crate the impl refers to (default `::persistant::relational`).
#[proc_macro_derive(FromRow, attributes(persistant))]
pub fn derive_from_row(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    imp::derive_from_row(input.into()).into()
}

/// `sqlx_query!`: sqlx's argument shape, run through UniSQL and PersistAnt (`docs/DESIGN-SQLX-BRIDGE.md`).
/// Result types come from sqlx's offline data (`.sqlx`).
#[proc_macro]
pub fn sqlx_query(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    sqlx_bridge::expand(sqlx_bridge::Form::Record, input.into()).into()
}

/// `sqlx_query_as!(Type, "SELECT ..", args..)`: as `sqlx_query!`, read into the caller's `FromRow` struct.
#[proc_macro]
pub fn sqlx_query_as(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    sqlx_bridge::expand(sqlx_bridge::Form::As, input.into()).into()
}

/// `sqlx_query_scalar!("SELECT ..", args..)`: as `sqlx_query!`, the single column.
#[proc_macro]
pub fn sqlx_query_scalar(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    sqlx_bridge::expand(sqlx_bridge::Form::Scalar, input.into()).into()
}
