// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed errors. No OpenDAL type appears here.

use std::fmt;

use crate::Need;

/// One thing a backend was asked for and cannot give, with the reason in words.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Missing {
    /// The declared need that is not met.
    pub need: Need,
    /// Why it is not met (for example, which setting is absent).
    pub detail: String,
}

/// Everything that can go wrong in PersistAnt.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The backend cannot give every declared need. Raised at open, before any data is touched.
    Refused {
        /// The backend that was refused, as a short name (`"fs"`, `"memory"`).
        backend: &'static str,
        /// Every unmet need, not just the first.
        missing: Vec<Missing>,
    },
    /// An operation was used whose need was never declared at open.
    NotDeclared(Need),
    /// The backend failed; the text is the backend's own message.
    Backend(String),
    /// Stored bytes are not a record envelope (not JSON, or fields missing).
    Corrupt(String),
    /// The stored record is of another kind than the one asked for.
    KindMismatch {
        /// Kind the program asked for.
        expected: &'static str,
        /// Kind found in storage.
        found: String,
    },
    /// The stored schema version is one the program cannot read: newer than it knows, or older
    /// with no upgrade path.
    SchemaUnsupported {
        /// The record kind.
        kind: &'static str,
        /// Schema version found in storage.
        found: u32,
        /// Schema version the program writes.
        expected: u32,
    },
    /// An older record was upgraded, but the result does not fit the current type. Either the
    /// stored data was damaged, or [`crate::Record::upgrade`] did not bring it all the way to
    /// `SCHEMA` (upgraded data carries no schema marker, so the two cannot be told apart).
    UpgradeFailed {
        /// The record kind.
        kind: &'static str,
        /// Schema version found in storage.
        from: u32,
        /// Schema version the upgrade had to reach.
        expected: u32,
        /// What the current type rejected.
        detail: String,
    },
    /// A record could not be turned into bytes.
    Encode(String),
    /// A failure produced on purpose by the fault-injecting fake.
    Injected(String),
    /// An operation panicked on the blocking facade's worker thread. The text is the panic
    /// message when it was a string, otherwise a placeholder. The store stays usable.
    Panicked(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Refused { backend, missing } => {
                write!(f, "backend `{backend}` refused:")?;
                for m in missing {
                    write!(f, " [{:?}: {}]", m.need, m.detail)?;
                }
                Ok(())
            }
            Error::NotDeclared(need) => write!(f, "{need:?} was not declared at open"),
            Error::Backend(msg) => write!(f, "backend error: {msg}"),
            Error::Corrupt(msg) => write!(f, "stored record is corrupt: {msg}"),
            Error::KindMismatch { expected, found } => {
                write!(f, "record kind `{found}` found, `{expected}` expected")
            }
            Error::SchemaUnsupported {
                kind,
                found,
                expected,
            } => write!(
                f,
                "record `{kind}` has schema {found}; this program writes {expected} and has no way to read {found}"
            ),
            Error::UpgradeFailed {
                kind,
                from,
                expected,
                detail,
            } => write!(
                f,
                "record `{kind}` upgraded from schema {from} does not fit schema {expected}: {detail}"
            ),
            Error::Encode(msg) => write!(f, "cannot encode record: {msg}"),
            Error::Injected(msg) => write!(f, "injected fault: {msg}"),
            Error::Panicked(msg) => write!(f, "operation panicked: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
