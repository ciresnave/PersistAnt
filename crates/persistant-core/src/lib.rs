// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's vocabulary and verification model. No I/O; the one dependency is `capant`, which reads and writes the matrix file.
//!
//! A **capability** is a boolean for one `(backend, version, config)`; it is off until an automated
//! test passed (`docs/DESIGN-KINDS.md` §2.1). This crate holds the pieces every kind shares:
//!
//! - [`CapabilityId`]: a validated string id, owned here, never changing meaning;
//! - [`Config`]: the `k=v;k=v` facts a gate can read at open;
//! - [`Row`] and [`Matrix`]: the recorded fact (§2.4) and the lookup the gate performs;
//! - [`Verdict`]: the four test outcomes, and the rule that an `on` row needs both controls;
//! - [`vocabulary::SEED`]: the seed ids with their written contracts (all off until tested).

mod adapter;
pub mod bench;
pub mod choose;
mod config;
mod id;
mod matrix;
pub mod provider;
mod row;
mod verdict;
pub mod vocabulary;

pub use config::{Config, ConfigError};
pub use id::{CapabilityId, IdError};
pub use matrix::{Lookup, Matrix};
pub use row::{ParseError, Reason, Row};
pub use verdict::{Controls, ControlsNotRun, Verdict};

/// The crate version, as declared in the workspace manifest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
