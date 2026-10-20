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
//! `source` (required), `targets` (required, at least one), `allow`, `requires`, `forbids`, and
//! `crate = path` (where the expansion finds `Query`, default `::persistant::embed`). The statement is
//! parsed in the source dialect and translated for every named target at build time; whatever cannot
//! be is a compile error. Nothing is looked up in a database and no schema is read yet (the
//! catalog-based checks are a later stage).

mod imp;

/// Check and translate a literal statement at build time. See the crate documentation.
#[proc_macro]
pub fn sql(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    imp::sql(input.into()).into()
}
