// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's relational kind: a **deliberately small** trait, no driver.
//!
//! `docs/DESIGN-KINDS.md` §3 rule 4 and D1 (C+): the trait runs a statement with parameters,
//! begins a transaction, and reports the dialect and server version. It is not a query builder or
//! a row mapper; a program that wants its own SQL checked at compile time (Humboldt's
//! `sqlx::query!`) uses the concrete driver handle the driver crate hands out, not this trait. The
//! trait exists so the key-value adapter, the capability matrix and migrations have one thing to
//! call.
//!
//! Growth toward statement construction would turn this into Option B (§4.2) and carry its leaks;
//! resist it.

mod backend;
#[cfg(feature = "sql")]
mod bound;
#[cfg(feature = "catalog")]
pub mod catalog;
pub mod choose;
pub mod conformance;
pub mod contract;
mod error;
mod migrate;
pub mod migrate_conformance;
#[cfg(feature = "sql")]
pub mod row;
mod secret;
pub mod speed;
#[cfg(feature = "sql")]
mod sql_ext;
mod sweep;
mod value;

pub use backend::{Isolation, RelationalBackend, RelationalTx};
pub use error::RelError;
pub use migrate::{MigrateError, MigrateReport, Migrates, Migration};
pub use secret::{ConnectionSecret, Redacted};
pub use sweep::{SweepError, SweepReport, sweep};
pub use value::{Dialect, Row, Value};

#[cfg(feature = "sql")]
pub use bound::{Bound, InTx, Runner};
#[cfg(feature = "sql")]
pub use row::{Field, FieldKind, FromRow, FromValue, ToValue, TypedSql};
#[cfg(feature = "sql")]
pub use sql_ext::{
    RelationalRowsExt, RelationalSqlExt, RelationalTxRowsExt, RelationalTxSqlExt, Sql,
};
/// `Query`, `Rendered`, `Target` and `Binding`: what `persistant::sql!` expands to (UniSQL's
/// `unisql-embed`, re-exported so a consumer depends on neither).
#[cfg(feature = "sql")]
pub use unisql_embed as embed;
