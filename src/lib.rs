// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt: a thin persistence layer over Apache OpenDAL, for blobs and records by key.
//!
//! Open a [`Store`] with a declaration of the [`Needs`] your program has; a backend that cannot
//! give them is refused with a typed [`Error`]. See the README for what this crate does and does
//! not cover.

mod error;
mod store;

pub use error::{Error, Missing, Result};
pub use store::{Config, Need, Needs, Replace, Store};

/// The crate version, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(VERSION, "0.2.0");
    }
}
