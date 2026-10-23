// SPDX-License-Identifier: MIT OR Apache-2.0
//! PersistAnt's blob driver over Apache OpenDAL.
//!
//! [`OpendalBlob`] implements [`persistant_blob::BlobBackend`] for OpenDAL's `fs` and `memory`
//! services. No OpenDAL type is part of this crate's public API. What the backend *gives* is not
//! read from OpenDAL's declared capability struct: it is whatever
//! `persistant_blob::conformance::certify` finds, recorded as matrix rows.
//!
//! `list` is emulated: OpenDAL lists directories, so every key under the prefix's directory part
//! is scanned and filtered, and the cost is a scan of that tree per page.

pub mod crash;
mod process_lock;

pub use process_lock::{
    ProcessLock, certify as certify_process_lock, maybe_run_child as maybe_run_lock_child,
};

use persistant_blob::{BlobBackend, BlobError, Entry, Page};
use persistant_core::Config;
use std::collections::BTreeSet;
use std::path::Path;

use opendal::{ErrorKind, Operator};

fn backend(e: opendal::Error) -> BlobError {
    BlobError::Backend(e.to_string())
}

/// A blob store on OpenDAL's `fs` or `memory` service.
#[derive(Debug)]
pub struct OpendalBlob {
    op: Operator,
    name: &'static str,
    config: Config,
    /// False for `fs` under `atomic_write_dir`: OpenDAL checks for the key and then renames, so
    /// two creators can both pass (README 0.4.6, 0.4.7). The driver says so instead of claiming it.
    exclusive_create: bool,
    /// False for `fs` without `atomic_write_dir`: the target is written in place.
    atomic_replace: bool,
}

impl OpendalBlob {
    /// In-process memory. Lost at exit.
    pub fn memory() -> Result<Self, BlobError> {
        let op = Operator::new(opendal::services::Memory::default()).map_err(backend)?;
        Ok(OpendalBlob {
            op,
            name: "opendal-memory",
            config: Config::new(),
            exclusive_create: true,
            atomic_replace: true,
        })
    }

    /// A directory on the local filesystem. `atomic_write_dir` is OpenDAL's scratch directory for
    /// its rename-based write path; it is a fact a matrix row is recorded under, because it
    /// changes what the backend gives (`docs/DESIGN-KINDS.md` §2.2, README 0.4.7).
    pub fn fs(root: &Path, atomic_write_dir: Option<&Path>) -> Result<Self, BlobError> {
        let mut b = opendal::services::Fs::default().root(&root.to_string_lossy());
        if let Some(dir) = atomic_write_dir {
            b = b.atomic_write_dir(&dir.to_string_lossy());
        }
        let op = Operator::new(b).map_err(backend)?;
        let config = Config::new().with(
            "atomic_write_dir",
            if atomic_write_dir.is_some() {
                "set"
            } else {
                "unset"
            },
        );
        Ok(OpendalBlob {
            op,
            name: "opendal-fs",
            config,
            exclusive_create: atomic_write_dir.is_none(),
            atomic_replace: atomic_write_dir.is_some(),
        })
    }

    /// The facts a matrix row for this backend is recorded under.
    pub fn config(&self) -> &Config {
        &self.config
    }
}

impl BlobBackend for OpendalBlob {
    fn backend(&self) -> &str {
        self.name
    }

    fn version(&self) -> &str {
        opendal::raw::VERSION
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        match self.op.read(key).await {
            Ok(buf) => Ok(Some(buf.to_vec())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        self.op.write(key, bytes).await.map(|_| ()).map_err(backend)
    }

    async fn delete(&self, key: &str) -> Result<(), BlobError> {
        self.op.delete(key).await.map_err(backend)
    }

    async fn stat(&self, key: &str) -> Result<Option<Entry>, BlobError> {
        match self.op.stat(key).await {
            Ok(m) if m.is_file() => Ok(Some(Entry::new(key, m.content_length()))),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Page, BlobError> {
        let dir = match prefix.rfind('/') {
            Some(i) => &prefix[..=i],
            None => "/",
        };
        // Keep the `limit + 1` smallest qualifying keys; the extra one says whether more follow.
        let mut kept: BTreeSet<String> = BTreeSet::new();
        let mut pending = vec![dir.to_string()];
        while let Some(d) = pending.pop() {
            let mut lister = match self.op.lister(&d).await {
                Ok(l) => l,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(backend(e)),
            };
            while let Some(entry) = futures_util::TryStreamExt::try_next(&mut lister)
                .await
                .map_err(backend)?
            {
                let path = entry.path();
                if path == d || path == "/" {
                    continue;
                }
                if path.ends_with('/') || entry.metadata().is_dir() {
                    pending.push(path.to_string());
                    continue;
                }
                if !path.starts_with(prefix) || after.is_some_and(|a| path <= a) {
                    continue;
                }
                kept.insert(path.to_string());
                if kept.len() > limit.saturating_add(1) {
                    kept.pop_last();
                }
            }
        }
        let mut entries = Vec::new();
        let mut more = false;
        for key in kept {
            if entries.len() == limit {
                more = true;
                break;
            }
            // A key that vanished between listing and stat is left out.
            if let Some(e) = self.stat(&key).await? {
                entries.push(e);
            }
        }
        let next = if more {
            entries.last().map(|e| e.key.clone())
        } else {
            None
        };
        Ok(Page::new(entries, next))
    }

    async fn create_exclusive(&self, key: &str, bytes: Vec<u8>) -> Result<bool, BlobError> {
        if !self.exclusive_create {
            return Err(BlobError::Unsupported(
                "create_exclusive under atomic_write_dir is check-then-rename".to_string(),
            ));
        }
        match self.op.write_with(key, bytes).if_not_exists(true).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == ErrorKind::ConditionNotMatch => Ok(false),
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                Err(BlobError::Unsupported(e.to_string()))
            }
            Err(e) => Err(backend(e)),
        }
    }

    async fn replace_atomic(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        if !self.atomic_replace {
            return Err(BlobError::Unsupported(
                "replace_atomic without atomic_write_dir writes in place".to_string(),
            ));
        }
        self.op.write(key, bytes).await.map(|_| ()).map_err(backend)
    }

    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        match self.op.write_with(key, bytes).append(true).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                Err(BlobError::Unsupported(e.to_string()))
            }
            Err(e) => Err(backend(e)),
        }
    }
}
