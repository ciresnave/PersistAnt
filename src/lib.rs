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
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(VERSION, "0.5.0");
    }
}
