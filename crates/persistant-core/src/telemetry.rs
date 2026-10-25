// SPDX-License-Identifier: MIT OR Apache-2.0
//! The one set of `tracing` field names every kind's traced wrapper uses (`docs/DESIGN-KINDS.md` §5).
//!
//! A span named [`SPAN`] carries [`KIND`], [`BACKEND`], [`OP`], [`CAPABILITY`] and, once the call has
//! returned, [`OUTCOME`]. **No key and no value content, and no error text**, is ever a field: keys
//! can be secrets, and an error message can quote one. Counters are the consumer's subscriber's job.

/// The span's name.
pub const SPAN: &str = "persist";
/// Field: the kind (`kv`, `blob`, `rel`).
pub const KIND: &str = "persist.kind";
/// Field: the backend's name, as `backend()` reports it.
pub const BACKEND: &str = "persist.backend";
/// Field: the operation (`get`, `put`, `batch`, ...).
pub const OP: &str = "persist.op";
/// Field: the capability id the operation exercises (`kv.get`, ...).
pub const CAPABILITY: &str = "persist.capability";
/// Field, recorded when the call returns: `ok`, or the error's kind (`backend`, `unsupported`,
/// `corrupt`, `timed_out`).
pub const OUTCOME: &str = "persist.outcome";
