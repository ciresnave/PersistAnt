// SPDX-License-Identifier: MIT OR Apache-2.0
//! The store: a backend opened against a declaration of what the program needs.

use std::path::PathBuf;

use opendal::Operator;

use crate::error::{Error, Missing, Result};

/// A guarantee a program declares it needs from its backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Need {
    /// Read a value by key.
    Read,
    /// Write a value by key.
    Write,
    /// Delete a key.
    Delete,
    /// Ask whether a key exists, and its size.
    Stat,
    /// List keys under a prefix.
    List,
    /// Write only if the key does not exist yet.
    CreateIfAbsent,
    /// Replace a value so a reader sees the old or the new content, never a mix, even across an
    /// interrupted write.
    AtomicReplace,
    /// Values up to this many bytes can be written in one piece.
    MaxValueSize(u64),
}

/// The set of needs a program declares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Needs(Vec<Need>);

impl Needs {
    /// An empty declaration.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a need.
    #[must_use]
    pub fn with(mut self, need: Need) -> Self {
        self.0.push(need);
        self
    }

    pub(crate) fn contains(&self, need: Need) -> bool {
        self.0.contains(&need)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Need> + '_ {
        self.0.iter().copied()
    }
}

/// Which backend to open and how it is configured.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Config {
    /// In-process memory. Lost at exit; for tests and caches.
    Memory,
    /// A directory on the local filesystem.
    Fs {
        /// Directory holding the keys.
        root: PathBuf,
        /// Scratch directory for atomic replace. Must be on the same filesystem as `root` and
        /// outside it. Without it, writes go straight to the target and are not atomic.
        atomic_write_dir: Option<PathBuf>,
    },
    /// Test fake: `inner`, pretending it cannot give the needs in `without`. Lets a test check that
    /// a program refuses a backend weaker than the one it was developed on.
    Masked {
        /// The backend actually used.
        inner: Box<Config>,
        /// Needs to report as missing.
        without: Vec<Need>,
    },
}

/// A backend opened against a declaration of needs.
#[derive(Debug)]
pub struct Store {
    op: Operator,
    declared: Needs,
}

impl Store {
    /// Open `config`, refusing it if it cannot give every need in `needs`.
    pub async fn open(config: Config, needs: Needs) -> Result<Store> {
        let (config, masked) = unmask(config);
        let (op, name) = match &config {
            Config::Memory => (
                Operator::new(opendal::services::Memory::default()),
                "memory",
            ),
            Config::Fs {
                root,
                atomic_write_dir,
            } => {
                let mut b = opendal::services::Fs::default().root(&root.to_string_lossy());
                if let Some(dir) = atomic_write_dir {
                    b = b.atomic_write_dir(&dir.to_string_lossy());
                }
                (Operator::new(b), "fs")
            }
            Config::Masked { .. } => unreachable!("unmask removes every Masked layer"),
        };
        let op = op.map_err(|e| Error::Backend(e.to_string()))?;
        let cap = op.info().capability();
        let atomic_dir_set = matches!(
            &config,
            Config::Fs {
                atomic_write_dir: Some(_),
                ..
            }
        );
        let missing: Vec<Missing> = needs
            .iter()
            .filter_map(|need| {
                let detail = if masked.contains(&need) {
                    Some("masked by the test fake".to_string())
                } else {
                    unmet(need, &cap, name, atomic_dir_set)
                };
                detail.map(|detail| Missing { need, detail })
            })
            .collect();
        if !missing.is_empty() {
            return Err(Error::Refused {
                backend: name,
                missing,
            });
        }
        Ok(Store {
            op,
            declared: needs,
        })
    }

    /// Read the whole value at `key`.
    pub async fn read(&self, key: &str) -> Result<Vec<u8>> {
        self.op
            .read(key)
            .await
            .map(|b| b.to_vec())
            .map_err(|e| Error::Backend(e.to_string()))
    }

    /// Replace the value at `key` in one step.
    pub async fn replace(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        let mut r = self.begin_replace(key).await?;
        r.write(bytes).await?;
        r.commit().await
    }

    /// Start a replacement that can be written in pieces. Dropping it without `commit` leaves the
    /// old value in place.
    pub async fn begin_replace(&self, key: &str) -> Result<Replace> {
        if !self.declared.contains(Need::AtomicReplace) {
            return Err(Error::NotDeclared(Need::AtomicReplace));
        }
        let w = self
            .op
            .writer(key)
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        Ok(Replace { w })
    }
}

/// Strip `Masked` layers, collecting what they hide.
fn unmask(mut config: Config) -> (Config, Vec<Need>) {
    let mut hidden = Vec::new();
    while let Config::Masked { inner, without } = config {
        hidden.extend(without);
        config = *inner;
    }
    (config, hidden)
}

/// Why `need` is not met by a backend with capability `cap`, or `None` if it is.
fn unmet(
    need: Need,
    cap: &opendal::Capability,
    backend: &str,
    atomic_dir_set: bool,
) -> Option<String> {
    let lacks = |ok: bool, what: &str| {
        (!ok).then(|| format!("backend `{backend}` does not advertise {what}"))
    };
    match need {
        Need::Read => lacks(cap.read, "read"),
        Need::Write => lacks(cap.write, "write"),
        Need::Delete => lacks(cap.delete, "delete"),
        Need::Stat => lacks(cap.stat, "stat"),
        Need::List => lacks(cap.list, "list"),
        Need::CreateIfAbsent => lacks(cap.write_with_if_not_exists, "write_with_if_not_exists"),
        Need::AtomicReplace => match backend {
            // fs writes straight to the target unless a scratch directory is configured.
            "fs" if !atomic_dir_set => {
                Some("fs writes atomically only with atomic_write_dir set; it is not".to_string())
            }
            // memory commits a value in one map insert on close.
            _ => None,
        },
        Need::MaxValueSize(n) => match cap.write_total_max_size {
            Some(max) if (max as u64) < n => {
                Some(format!("backend ceiling is {max} bytes, need {n}"))
            }
            _ => None,
        },
    }
}

/// An in-progress replacement of one key.
pub struct Replace {
    w: opendal::Writer,
}

impl std::fmt::Debug for Replace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replace").finish_non_exhaustive()
    }
}

impl Replace {
    /// Append a piece to the new value.
    pub async fn write(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.w
            .write(bytes)
            .await
            .map_err(|e| Error::Backend(e.to_string()))
    }

    /// Make the new value visible.
    pub async fn commit(mut self) -> Result<()> {
        self.w
            .close()
            .await
            .map(|_| ())
            .map_err(|e| Error::Backend(e.to_string()))
    }
}
