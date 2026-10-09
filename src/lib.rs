// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt: a thin persistence layer over Apache OpenDAL, for blobs and records by key.
//!
#![doc = include_str!("../README.md")]

pub mod blocking;
mod error;
mod store;

pub use error::{Error, Missing, Result};
pub use store::{Config, Faults, Need, Needs, Record, Replace, Store};

/// The crate version, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(VERSION, "0.4.2");
    }
}
