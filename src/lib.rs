// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt: a thin persistence layer over Apache OpenDAL, for blobs and records by key.
//!
//! This is the skeleton release. The capability check and the atomic-replace helper arrive in
//! the next change; see the README for what this crate does and does not cover.

/// The crate version, as declared in `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_the_manifest_version() {
        assert_eq!(VERSION, "0.1.0");
    }
}
