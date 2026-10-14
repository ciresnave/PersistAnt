// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's key-value kind: a trait, adapters, a reference backend, and the tests that decide
//! capabilities. No driver is linked here (`docs/DESIGN-KINDS.md` §3).
//!
//! - [`KvBackend`]: get, put (with optional time to live), delete, put-if-absent, prefix listing
//!   and ordered scan. A method a backend cannot honour returns [`KvError::Unsupported`]; the
//!   capability stays off because its test did not pass.
//! - [`KvOnBlob`]: a wrapper type (not a blanket impl) that builds a [`KvBackend`] on any
//!   [`persistant_blob::BlobBackend`]. What it *gives* is derived as eligible and then tested per
//!   `(blob backend, version, config)`.
//! - [`KvOnRelational`]: the same for any [`persistant_relational::RelationalBackend`] (D1, C+),
//!   PostgreSQL SQL for now, keys as `BYTEA`, tested per server and version.
//! - [`MemoryKv`]: a reference backend with injectable [`Defect`]s for known-bad controls.
//! - [`conformance`]: the contract tests, producing matrix rows.
//!
//! Not offered yet, so off everywhere: `kv.compare_and_swap`, `kv.take`, `kv.batch_atomic`,
//! `kv.ttl_remaining`, `kv.count`. `KvOnBlob` cannot give the first three on a blob store without
//! compare-and-delete; see its docs.

mod backend;
pub mod conformance;
mod lease;
mod memory;
mod on_blob;
mod on_relational;

pub use backend::{KeyPage, KvBackend, KvError};
pub use lease::Lease;
pub use memory::{Defect, MemoryKv};
pub use on_blob::KvOnBlob;
pub use on_relational::KvOnRelational;
