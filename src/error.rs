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
        }
    }
}

impl std::error::Error for Error {}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, Error>;
