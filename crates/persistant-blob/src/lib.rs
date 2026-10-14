// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's blob kind: a trait, a reference backend, and the tests that decide capabilities.
//!
//! No driver is linked here (`docs/DESIGN-KINDS.md` §3 rule 1). A driver crate such as
//! `persistant-opendal` implements [`BlobBackend`]; [`conformance`] runs the contract tests against
//! it and returns matrix rows, each `on` only with both controls.
//!
//! The harness covers the leaves whose written contract it can decide: sequentially
//! (`blob.read`, `blob.write`, `blob.delete`, `blob.stat`, `blob.list`) and by sampled
//! concurrency (`blob.create_exclusive`, `blob.replace_atomic.concurrent_read`; the trial
//! parameters are recorded in the row). Leaves that need a crash or a power cut
//! (`blob.replace_atomic` as a whole, `blob.durable`) have **no test here yet**, so no row is
//! produced and they stay off.

mod backend;
pub mod concurrency;
pub mod conformance;
mod memory;

pub use backend::{BlobBackend, BlobError, Entry, Page};
pub use memory::{Defect, MemoryBlob};
