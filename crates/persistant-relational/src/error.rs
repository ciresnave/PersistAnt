// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;

/// Why a relational operation failed, classified so a caller can branch without parsing driver
/// text (Humboldt hand-maps constraint names in at least seven places today). Plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelError {
    /// A unique or primary-key constraint rejected the statement.
    UniqueViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
    },
    /// A foreign-key constraint rejected the statement.
    ForeignKeyViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
    },
    /// A check constraint (or NOT NULL) rejected the statement.
    CheckViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
    },
    /// The transaction could not be serialised; retrying it may succeed.
    Serialization,
    /// The server chose this transaction as a deadlock victim; retrying it may succeed.
    Deadlock,
    /// The server could not be reached or went away.
    Unavailable(String),
    /// The backend cannot do this, and says so rather than approximating.
    Unsupported(String),
    /// Any other failure; the text is the driver's.
    Backend(String),
}

impl fmt::Display for RelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = |c: &Option<String>| c.as_deref().unwrap_or("?").to_string();
        match self {
            RelError::UniqueViolation { constraint } => {
                write!(f, "unique violation ({})", c(constraint))
            }
            RelError::ForeignKeyViolation { constraint } => {
                write!(f, "foreign key violation ({})", c(constraint))
            }
            RelError::CheckViolation { constraint } => {
                write!(f, "check violation ({})", c(constraint))
            }
            RelError::Serialization => f.write_str("serialization failure"),
            RelError::Deadlock => f.write_str("deadlock detected"),
            RelError::Unavailable(m) => write!(f, "database unavailable: {m}"),
            RelError::Unsupported(m) => write!(f, "database does not support: {m}"),
            RelError::Backend(m) => write!(f, "database: {m}"),
        }
    }
}

impl std::error::Error for RelError {}
