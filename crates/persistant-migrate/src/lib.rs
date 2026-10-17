// SPDX-License-Identifier: MIT OR Apache-2.0
//! The logic behind the `persistant-migrate` command: reading a directory of migrations and comparing
//! it with a database ledger. The commands themselves are in `main.rs`.
//!
//! A directory holds `NNN_name.up.sql` files (with `NNN_name.down.sql` partners) or plain
//! `NNN_name.sql`. The ledger is sqlx's own `_sqlx_migrations`, so a database can be handled by this
//! tool or by `sqlx migrate run` interchangeably. **Down files are read and checked for pairing, never
//! run**: [`Migrates`](persistant_relational::Migrates) has no undo.

use persistant_relational::Migration;
use sha2::{Digest, Sha384};
use std::path::Path;

/// Why a directory could not be turned into migrations.
#[derive(Debug)]
pub enum LoadError {
    /// The directory or a file in it could not be read or parsed.
    Read(String),
    /// A `.down.sql` file has no `.up.sql` partner of the same version.
    UnpairedDown {
        /// The version.
        version: i64,
    },
    /// The migrations cannot be run as they stand (see [`persistant_sqlx::FromSqlxError`]).
    Unsupported(persistant_sqlx::FromSqlxError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Read(m) => write!(f, "reading the migrations: {m}"),
            LoadError::UnpairedDown { version } => {
                write!(f, "migration {version} has a .down.sql but no .up.sql")
            }
            LoadError::Unsupported(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// The migrations in `dir`, in version order. Each down file must pair with an up file.
pub async fn load_dir(dir: &Path) -> Result<Vec<Migration>, LoadError> {
    let entries = std::fs::read_dir(dir).map_err(|e| LoadError::Read(e.to_string()))?;
    let mut ups = Vec::new();
    let mut downs = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(|e| LoadError::Read(e.to_string()))?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let version = name.split('_').next().and_then(|v| v.parse::<i64>().ok());
        if let Some(v) = version {
            if name.ends_with(".down.sql") {
                downs.push(v);
            } else if name.ends_with(".sql") {
                ups.push(v);
            }
        }
    }
    if let Some(&version) = downs.iter().find(|d| !ups.contains(d)) {
        return Err(LoadError::UnpairedDown { version });
    }
    let migrator = sqlx::migrate::Migrator::new(dir)
        .await
        .map_err(|e| LoadError::Read(e.to_string()))?;
    persistant_sqlx::migrations_from_sqlx(&migrator).map_err(LoadError::Unsupported)
}

/// The checksum the ledger holds for `m`: SHA-384 of its text.
pub fn checksum(m: &Migration) -> Vec<u8> {
    Sha384::digest(m.sql.as_bytes()).to_vec()
}

/// Where one migration stands against a database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Applied, and the text still matches.
    Applied,
    /// In the directory, not yet applied.
    Pending,
    /// Applied, but the text in the directory is not what was applied.
    Changed,
    /// Applied in the database, absent from the directory: the database is newer than this program.
    Unknown,
}

/// One line of a status report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The version.
    pub version: i64,
    /// The description (empty for an [`State::Unknown`] version).
    pub description: String,
    /// Where it stands.
    pub state: State,
}

/// Compare `migrations` with the ledger rows `(version, checksum)`.
pub fn status(migrations: &[Migration], ledger: &[(i64, Vec<u8>)]) -> Vec<Line> {
    let mut out: Vec<Line> = migrations
        .iter()
        .map(|m| {
            let state = match ledger.iter().find(|(v, _)| *v == m.version) {
                None => State::Pending,
                Some((_, c)) if *c == checksum(m) => State::Applied,
                Some(_) => State::Changed,
            };
            Line {
                version: m.version,
                description: m.description.clone(),
                state,
            }
        })
        .collect();
    out.extend(
        ledger
            .iter()
            .filter(|(v, _)| !migrations.iter().any(|m| m.version == *v))
            .map(|(v, _)| Line {
                version: *v,
                description: String::new(),
                state: State::Unknown,
            }),
    );
    out.sort_by_key(|l| l.version);
    out
}
