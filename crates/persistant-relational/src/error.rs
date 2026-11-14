// SPDX-License-Identifier: MIT OR Apache-2.0
use std::fmt;

/// Why a relational operation failed, classified so a caller can branch without parsing driver
/// text (Humboldt hand-maps constraint names in at least seven places today). Plain data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RelError {
    /// A unique or primary-key constraint rejected the statement.
    #[non_exhaustive]
    UniqueViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
        /// What the server reported (code, message, ...), when the driver fills it (boxed to keep
        /// `RelError` small; read it with [`RelError::server_error`]).
        server: Option<Box<ServerError>>,
    },
    /// A foreign-key constraint rejected the statement.
    #[non_exhaustive]
    ForeignKeyViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
        /// What the server reported (code, message, ...), when the driver fills it (boxed to keep
        /// `RelError` small; read it with [`RelError::server_error`]).
        server: Option<Box<ServerError>>,
    },
    /// A check constraint (or NOT NULL) rejected the statement.
    #[non_exhaustive]
    CheckViolation {
        /// The constraint's name, if the server reports it.
        constraint: Option<String>,
        /// What the server reported (code, message, ...), when the driver fills it (boxed to keep
        /// `RelError` small; read it with [`RelError::server_error`]).
        server: Option<Box<ServerError>>,
    },
    /// The transaction could not be serialised; retrying it may succeed.
    Serialization,
    /// The server chose this transaction as a deadlock victim; retrying it may succeed.
    Deadlock,
    /// The server could not be reached or went away.
    Unavailable(String),
    /// The backend cannot do this, and says so rather than approximating.
    Unsupported(String),
    /// A call did not finish within the configured per-call bound and was cut off. For a statement
    /// outside a transaction, whether it took effect is unknown; a transaction that saw one is
    /// rolled back and refuses to commit.
    Timeout {
        /// The bound that expired.
        after: std::time::Duration,
    },
    /// A database error the classifier has no category of its own for (a trigger's `RAISE`, any
    /// other SQLSTATE): the server's code and message are kept in `server`.
    #[non_exhaustive]
    Database {
        /// The constraint's name, if the server reports one.
        constraint: Option<String>,
        /// What the server reported (code, message, ...), when the driver fills it (boxed to keep
        /// `RelError` small; read it with [`RelError::server_error`]).
        server: Option<Box<ServerError>>,
    },
    /// Any other failure that did not come from the database server (decode, bind, protocol); the
    /// text is the driver's.
    Backend(String),
}

/// What the database server said about a failed statement: its SQLSTATE and message, and the
/// table, column and detail it names when it names them. The constraint name is not here: it lives
/// once, in the [`RelError`] variant's `constraint`. Plain data; build one with [`ServerError::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ServerError {
    /// The five-character SQLSTATE, such as `23505` or a custom code a trigger raised.
    pub sqlstate: String,
    /// The server's primary message.
    pub message: String,
    /// The table the error names, if any.
    pub table: Option<String>,
    /// The column the error names, if any.
    pub column: Option<String>,
    /// The server's detail line, if any.
    pub detail: Option<String>,
}

impl ServerError {
    /// A server error with its SQLSTATE and message; add the rest with the `with_` methods.
    #[must_use]
    pub fn new(sqlstate: impl Into<String>, message: impl Into<String>) -> Self {
        ServerError {
            sqlstate: sqlstate.into(),
            message: message.into(),
            table: None,
            column: None,
            detail: None,
        }
    }

    /// Set the table the error names.
    #[must_use]
    pub fn with_table(mut self, table: Option<String>) -> Self {
        self.table = table;
        self
    }

    /// Set the column the error names.
    #[must_use]
    pub fn with_column(mut self, column: Option<String>) -> Self {
        self.column = column;
        self
    }

    /// Set the server's detail line.
    #[must_use]
    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail;
        self
    }
}

impl RelError {
    /// A unique or primary-key violation (the variant is `#[non_exhaustive]`, so a driver crate builds
    /// it here).
    #[must_use]
    pub fn unique_violation(constraint: Option<String>, server: Option<ServerError>) -> Self {
        let server = server.map(Box::new);
        RelError::UniqueViolation { constraint, server }
    }

    /// A foreign-key violation.
    #[must_use]
    pub fn foreign_key_violation(constraint: Option<String>, server: Option<ServerError>) -> Self {
        let server = server.map(Box::new);
        RelError::ForeignKeyViolation { constraint, server }
    }

    /// A check or NOT NULL violation.
    #[must_use]
    pub fn check_violation(constraint: Option<String>, server: Option<ServerError>) -> Self {
        let server = server.map(Box::new);
        RelError::CheckViolation { constraint, server }
    }

    /// A database error with no category of its own.
    #[must_use]
    pub fn database(constraint: Option<String>, server: Option<ServerError>) -> Self {
        let server = server.map(Box::new);
        RelError::Database { constraint, server }
    }

    /// What the server reported, for the four variants that carry it; `None` for every other error
    /// (a decode or bind failure never reached the server) and for a driver that does not fill it.
    #[must_use]
    pub fn server_error(&self) -> Option<&ServerError> {
        match self {
            RelError::UniqueViolation { server, .. }
            | RelError::ForeignKeyViolation { server, .. }
            | RelError::CheckViolation { server, .. }
            | RelError::Database { server, .. } => server.as_deref(),
            _ => None,
        }
    }

    /// The constraint's name, for the four variants that carry one: the same value a caller reads
    /// from the variant's own `constraint` field.
    #[must_use]
    pub fn constraint(&self) -> Option<&str> {
        match self {
            RelError::UniqueViolation { constraint, .. }
            | RelError::ForeignKeyViolation { constraint, .. }
            | RelError::CheckViolation { constraint, .. }
            | RelError::Database { constraint, .. } => constraint.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for RelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = |c: &Option<String>| c.as_deref().unwrap_or("?").to_string();
        match self {
            RelError::UniqueViolation { constraint, .. } => {
                write!(f, "unique violation ({})", c(constraint))
            }
            RelError::ForeignKeyViolation { constraint, .. } => {
                write!(f, "foreign key violation ({})", c(constraint))
            }
            RelError::CheckViolation { constraint, .. } => {
                write!(f, "check violation ({})", c(constraint))
            }
            RelError::Serialization => f.write_str("serialization failure"),
            RelError::Deadlock => f.write_str("deadlock detected"),
            RelError::Unavailable(m) => write!(f, "database unavailable: {m}"),
            RelError::Unsupported(m) => write!(f, "database does not support: {m}"),
            RelError::Timeout { after } => write!(f, "database call timed out after {after:?}"),
            RelError::Database { constraint, server } => match server {
                Some(e) => write!(f, "database error {}: {}", e.sqlstate, e.message),
                None => write!(f, "database error ({})", c(constraint)),
            },
            RelError::Backend(m) => write!(f, "database: {m}"),
        }
    }
}

impl std::error::Error for RelError {}
