// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::RelError;
use std::fmt;
use std::path::PathBuf;

/// A connection string that never appears in a `Debug` print, a log line or an error.
///
/// A DSN usually carries a password. Held as a plain `String` in a `#[derive(Debug)]` config it is
/// one `{:?}` away from a log file. This type is where a connection string lives instead: a
/// *reference* to where it is (an environment variable, a file) or, where a program must, the
/// literal, which prints as `<redacted>`. The reference variants print their *name*, which is not
/// a secret.
///
/// Resolution happens at open ([`ConnectionSecret::resolve`]); the resolved value is a
/// [`Redacted`] that prints as `<redacted>` and gives up its text only through
/// [`Redacted::expose`], a call a reviewer can grep for.
#[derive(Clone, PartialEq, Eq)]
pub enum ConnectionSecret {
    /// Read the connection string from this environment variable when opening.
    Env(String),
    /// Read it from this file (trailing whitespace trimmed), as a mounted secret.
    File(PathBuf),
    /// The literal. Prefer a reference; this exists for tests and for programs that already
    /// hold the string.
    Literal(Redacted),
}

impl ConnectionSecret {
    /// A literal connection string.
    pub fn literal(s: impl Into<String>) -> Self {
        ConnectionSecret::Literal(Redacted::new(s))
    }

    /// Resolve to the connection string. Errors name the variable or file, never a value.
    pub fn resolve(&self) -> Result<Redacted, RelError> {
        match self {
            ConnectionSecret::Env(name) => std::env::var(name)
                .map(Redacted::new)
                .map_err(|_| RelError::Backend(format!("environment variable {name} is not set"))),
            ConnectionSecret::File(path) => std::fs::read_to_string(path)
                .map(|s| Redacted::new(s.trim_end().to_string()))
                .map_err(|e| {
                    RelError::Backend(format!(
                        "connection secret file {} could not be read: {}",
                        path.display(),
                        e.kind()
                    ))
                }),
            ConnectionSecret::Literal(r) => Ok(r.clone()),
        }
    }
}

impl fmt::Debug for ConnectionSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectionSecret::Env(n) => write!(f, "ConnectionSecret::Env({n:?})"),
            ConnectionSecret::File(p) => write!(f, "ConnectionSecret::File({p:?})"),
            ConnectionSecret::Literal(_) => f.write_str("ConnectionSecret::Literal(<redacted>)"),
        }
    }
}

/// A string that prints as `<redacted>`; its text comes out only through [`Redacted::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct Redacted(String);

impl Redacted {
    /// Wrap a secret string.
    pub fn new(s: impl Into<String>) -> Self {
        Redacted(s.into())
    }

    /// The text. The one call that reveals it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "postgres://u:hunter2@db/x";

    #[test]
    fn nothing_prints_the_literal() {
        let c = ConnectionSecret::literal(SECRET);
        assert!(!format!("{c:?}").contains("hunter2"));
        let r = c.resolve().unwrap();
        assert!(!format!("{r:?} {r}").contains("hunter2"));
        assert_eq!(
            r.expose(),
            SECRET,
            "control: the text is still reachable on purpose"
        );
    }

    #[test]
    fn a_reference_prints_its_name_and_resolution_errors_never_print_a_value() {
        let e = ConnectionSecret::Env("PERSISTANT_TEST_SURELY_UNSET_DSN".to_string());
        assert!(format!("{e:?}").contains("PERSISTANT_TEST_SURELY_UNSET_DSN"));
        let err = e.resolve().unwrap_err().to_string();
        assert!(err.contains("PERSISTANT_TEST_SURELY_UNSET_DSN"), "{err}");
    }

    #[test]
    fn a_file_secret_is_trimmed_and_unreadable_files_name_the_path_only() {
        let dir = std::env::temp_dir().join(format!("persistant-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("dsn");
        std::fs::write(&p, format!("{SECRET}\n")).unwrap();
        let r = ConnectionSecret::File(p.clone()).resolve().unwrap();
        assert_eq!(r.expose(), SECRET);
        std::fs::remove_dir_all(&dir).unwrap();
        let err = ConnectionSecret::File(p).resolve().unwrap_err().to_string();
        assert!(!err.contains("hunter2"), "{err}");
    }
}
