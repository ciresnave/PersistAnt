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
//!   PostgreSQL SQL translated by UniSQL (PostgreSQL, SQLite), keys as bytes, tested per server and version.
//! - [`copy`]: copy every pair of one store into another, read it back, and report progress in pairs.
//! - [`index::Indexed`]: secondary-key lookup as a layer over `batch` and `ordered_scan_values` (leaf `kv.index`).
//! - [`MemoryKv`]: a reference backend with injectable [`Defect`]s for known-bad controls.
//! - [`choose`]: stage 4, "best backend for X": a ranked list with its evidence, from the contract,
//!   a stated durability minimum and measurements in one environment; never a silent choice.
//! - [`conformance`]: the contract tests, producing matrix rows.
//! - [`contract`]: pass/fail eligibility for a set of required leaves; a failing backend is excluded,
//!   never ranked slower.
//! - [`speed`]: stage 3, timings as data with the environment recorded; only backends that pass the
//!   contract get a number, and nothing is ranked across machines.
//! - [`stats`]: stage 2, how often each leaf passes and what each operation did (no timing).
//! - `Traced` (feature `tracing`): a wrapper that emits one `persist` span per call, with the field
//!   names of `persistant_core::telemetry` and no key, value or error text.
//!
//! `kv.take` (get-and-delete, one winner) is built on `compare_and_delete`, so a backend has it
//! exactly when it has that. `kv.count` (live keys under a prefix, from one snapshot) is native on
//! memory and the SQL engines and off on a blob store. `KvOnBlob` cannot give `compare_and_swap`, `take`
//! or `batch_atomic` on a blob store without compare-and-delete; see its docs.

mod backend;
pub mod choose;
pub mod conformance;
pub mod contract;
pub mod copy;
pub mod durability;
mod fault;
pub mod index;
mod lease;
mod memory;
mod on_blob;
mod on_relational;
pub mod speed;
pub mod stats;
#[cfg(feature = "tracing")]
mod traced;
mod translate;

pub use backend::{
    BatchOp, BoundedProbe, KeyPage, KvBackend, KvError, ValuePage, count_prefix_paged,
};
pub use fault::Faulty;
pub use lease::Lease;
pub use memory::{Defect, MemoryKv};
pub use on_blob::KvOnBlob;
pub use on_relational::KvOnRelational;
#[cfg(feature = "tracing")]
pub use traced::Traced;
