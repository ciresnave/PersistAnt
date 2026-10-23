// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt: a thin persistence layer over Apache OpenDAL, for blobs and records by key.
//!
#![doc = include_str!("../README.md")]

pub mod blocking;
mod error;
mod store;

pub use error::{Error, Missing, Result};
pub use store::{Config, Entry, Faults, Need, Needs, Page, Record, Replace, Store};

// The other kinds of persistence, behind features that are off by default. Additive: nothing here
// changes `Store`, `Config`, `Need` or any other item above. Names avoid `core` and `opendal`
// because those are already names in scope here.

/// The capability vocabulary, the matrix file and its gate lookup (`persistant-core`).
#[cfg(feature = "core")]
pub use persistant_core as capability;

/// The blob trait, its reference backend and its contract tests (`persistant-blob`).
#[cfg(feature = "blob")]
pub use persistant_blob as blob;

/// The key-value trait, `KvOnBlob`, `KvOnRelational`, leases and their contract tests
/// (`persistant-kv`).
#[cfg(feature = "kv")]
pub use persistant_kv as kv;

/// The small relational trait, values and typed errors (`persistant-relational`).
#[cfg(feature = "relational")]
pub use persistant_relational as relational;

/// The OpenDAL blob driver and `ProcessLock` (`persistant-opendal`).
#[cfg(feature = "opendal-driver")]
pub use persistant_opendal as opendal_driver;

/// The PostgreSQL driver over sqlx (`persistant-sqlx`). Needs Rust 1.94.
#[cfg(feature = "sqlx-driver")]
pub use persistant_sqlx as sqlx_driver;

/// The crate version, as declared in `Cargo.toml`.
#[cfg(feature = "registry")]
pub mod registry;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Compile-time SQL (feature `sql-macros`): `sql!` checks a literal statement and translates it for
/// every target named, at build time. The syntax, the placeholders and every target's translation are
/// build errors, not run-time ones; no database is consulted and no schema is read yet (the
/// catalog-based checks are a later stage, `docs/COMPILE-TIME-SQL.md`).
///
/// `schema` is required: `schema = none` for database-free checks only, or a path (relative to your
/// `Cargo.toml`) to a `.json` snapshot written by `persistant-migrate snapshot`, or a `.sql` DDL file.
/// With none stated the crate looks for `persistant-schema.json` and fails to build without it: a
/// missing schema is never a quiet downgrade. With one, UniSQL checks the names and the `INSERT`
/// arity, and PersistAnt checks that an `INSERT` does not leave out a `NOT NULL` column that has no
/// default and that an `ON CONFLICT (..)` names a unique key. It errors only when certain: what the
/// schema does not state is unknown and passes.
///
/// ```
/// use persistant::{Sql, sql};
/// const FIND: Sql = sql!(
///     source = postgres, targets = [postgres, sqlite], schema = none,
///     "SELECT id FROM users WHERE id = $1"
/// );
/// assert_eq!(FIND.params, 1);
/// ```
///
/// A statement that cannot be translated for a named target does not build. The twin above is the same
/// call with the fault (`md5` has no SQLite form) removed:
///
/// ```
/// use persistant::{Sql, sql};
/// const OK: Sql = sql!(source = postgres, targets = [postgres], schema = none, "SELECT md5($1)");
/// ```
///
/// ```compile_fail
/// use persistant::{Sql, sql};
/// const BAD: Sql = sql!(source = postgres, targets = [postgres, sqlite], schema = none, "SELECT md5($1)");
/// ```
///
/// Against a schema (`tests/doctest-schema.sql`: `users` has `id` and `email`, both `NOT NULL` with no
/// default), an `INSERT` that leaves `email` out does not build; the twin that supplies it does:
///
/// ```
/// use persistant::{Sql, sql};
/// const OK: Sql = sql!(
///     source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "INSERT INTO users (id, email) VALUES ($1, $2)"
/// );
/// ```
///
/// ```compile_fail
/// use persistant::{Sql, sql};
/// const BAD: Sql = sql!(
///     source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "INSERT INTO users (id) VALUES ($1)"
/// );
/// ```
#[cfg(feature = "sql-macros")]
pub use persistant_macros::sql;

/// The sqlx bridge (feature `sqlx-query`): `sqlx_query!("SELECT ..", args..)`, `sqlx_query_as!(Type,
/// "SELECT ..", args..)` and `sqlx_query_scalar!("SELECT ..", args..)` take sqlx's arguments and return a
/// [`Bound`] statement; `.fetch_one(&db)`, `.fetch_optional(&db)`, `.fetch_all(&db)` and `.execute(&db)`
/// run it on a PersistAnt backend (or `InTx::new(&mut tx, db.dialect())` in a transaction). The result
/// types are read from sqlx's offline data (`.sqlx/query-<sha256>.json`, from `cargo sqlx prepare`),
/// never from a second schema, and the statement is translated by UniSQL for `targets = [..]`
/// (default `[postgres]`). With feature `sqlx-query-checked` each literal is also checked by sqlx's own
/// macro, in a closure that is never called. Options come first and are all optional: `targets = [..]`,
/// `allow = [..]` (as for `sql!`), `crate = ..` and `sqlx = ..`.
/// See `docs/DESIGN-SQLX-BRIDGE.md`.
#[cfg(feature = "sqlx-query")]
pub use persistant_macros::{sqlx_query, sqlx_query_as, sqlx_query_scalar};

/// What `sql!` expands to, and the extension traits that run it (feature `sql-macros`).
#[cfg(feature = "sql-macros")]
pub use persistant_relational::{
    Bound, InTx, RelationalRowsExt, RelationalSqlExt, RelationalTxRowsExt, RelationalTxSqlExt,
    Runner, Sql, TypedSql, embed,
};

/// Typed rows (feature `sql-macros`): `#[derive(FromRow)]` reads a struct from a result row by
/// position, and `sql!(as T, ..)` checks a `SELECT` against it **at build time**, using the schema: the
/// number of columns, each column's name (the alias, if any), its class (a numeric column into an
/// integer or float field, text into `String`, a boolean into `bool`) and its nullability (a column
/// that can be NULL needs an `Option` field). A mismatch is a build error naming the column. Run the
/// result with `query_as`.
///
/// ```
/// use persistant::{FromRow, TypedSql, sql};
/// #[derive(FromRow)]
/// struct User { id: i64, email: String, note: Option<String> }
/// const FIND: TypedSql<User> = sql!(
///     as User, source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "SELECT id, email, note FROM users WHERE id = $1"
/// );
/// ```
///
/// An expression column is named and typed: `count(*) AS "n: i64"` (or `Option<i64>`). The twins of
/// the failing cases below are the example above:
///
/// ```compile_fail
/// use persistant::{FromRow, TypedSql, sql};
/// #[derive(FromRow)]
/// struct User { id: i64, email: i64, note: Option<String> }   // `email` is text
/// const FIND: TypedSql<User> = sql!(
///     as User, source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "SELECT id, email, note FROM users WHERE id = $1"
/// );
/// ```
///
/// ```compile_fail
/// use persistant::{FromRow, TypedSql, sql};
/// #[derive(FromRow)]
/// struct User { id: i64, email: String, note: String }   // `note` can be NULL
/// const FIND: TypedSql<User> = sql!(
///     as User, source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "SELECT id, email, note FROM users WHERE id = $1"
/// );
/// ```
///
/// ```compile_fail
/// use persistant::{FromRow, TypedSql, sql};
/// #[derive(FromRow)]
/// struct User { id: i64, mail: String, note: Option<String> }   // the column is `email`
/// const FIND: TypedSql<User> = sql!(
///     as User, source = postgres, targets = [sqlite], schema = "tests/doctest-schema.sql",
///     "SELECT id, email, note FROM users WHERE id = $1"
/// );
/// ```
#[cfg(feature = "sql-macros")]
pub use persistant_macros::FromRow;

/// The trait `#[derive(FromRow)]` implements (feature `sql-macros`).
#[cfg(feature = "sql-macros")]
pub use persistant_relational::FromRow;

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(VERSION, "0.9.25");
    }
}
