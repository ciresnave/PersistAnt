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
mod file_mode;
mod plaintext;
mod process_lock;

pub use file_mode::{certify_file_mode, owner_only_reading};
pub use plaintext::{certify_no_plaintext_at_rest, find_canary};
pub use process_lock::{
    ProcessLock, certify as certify_process_lock, maybe_run_child as maybe_run_lock_child,
};

use persistant_blob::{BlobBackend, BlobError, BlobSink, BoxFuture, Entry, KeyLayout, Page};
use persistant_core::Config;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use opendal::{ErrorKind, Operator};

fn backend(e: opendal::Error) -> BlobError {
    BlobError::Backend(e.to_string())
}

#[derive(Debug)]
struct FsDirs {
    root: PathBuf,
    scratch: Option<PathBuf>,
}

/// A blob store on OpenDAL's `fs` or `memory` service.
#[derive(Debug)]
pub struct OpendalBlob {
    op: Operator,
    name: &'static str,
    config: Config,
    /// Where `fs` keeps its files and, when `atomic_write_dir` is set, its scratch files. Exclusive
    /// create under a scratch directory is done here, not by OpenDAL (whose own is
    /// check-then-rename, README 0.4.6, 0.4.7): see [`OpendalBlob::create_exclusive`].
    fs: Option<FsDirs>,
    /// False for `fs` without `atomic_write_dir`: the target is written in place.
    atomic_replace: bool,
    /// Every file this driver creates on `fs` is owner-only (see [`OpendalBlob::owner_only`]).
    owner_only: bool,
}

impl OpendalBlob {
    /// In-process memory. Lost at exit.
    pub fn memory() -> Result<Self, BlobError> {
        let op = Operator::new(opendal::services::Memory::default()).map_err(backend)?;
        Ok(OpendalBlob {
            op,
            name: "opendal-memory",
            config: Config::new(),
            fs: None,
            atomic_replace: true,
            owner_only: false,
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
        if let Some(dir) = atomic_write_dir {
            clean_staging(dir);
        }
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
            fs: Some(FsDirs {
                root: root.to_path_buf(),
                scratch: atomic_write_dir.map(Path::to_path_buf),
            }),
            atomic_replace: atomic_write_dir.is_some(),
            owner_only: false,
        })
    }

    /// Make every file this driver creates under an `fs` root owner-only (`blob.file_mode`): mode
    /// `0600` on Unix whatever the umask, and a directory it has to create `0700`; on Windows a
    /// protected ACL with one entry, for the user the process runs as. A file that already exists
    /// keeps the mode its owner gave it. The matrix row is recorded under the config fact
    /// `file_mode=owner_only`, so a program that asks for owner-only files finds this driver's row
    /// and one that does not is not told it has them.
    ///
    /// The file is created owner-only before any content is written, never restricted afterwards:
    /// writes and replaces under a scratch directory are staged by hand (as `write_stream` is), and
    /// an exclusive create links an owner-only scratch file. **Not covered:** directories on
    /// Windows (they inherit their parent's ACL; the files in them do not), and the scratch
    /// directory's own listing. On the memory service this is a no-op: there are no files.
    #[must_use]
    pub fn owner_only(mut self) -> Self {
        if self.fs.is_some() {
            self.owner_only = true;
            self.config = self.config.with("file_mode", "owner_only");
        }
        self
    }

    /// For an owner-only `fs`: create the key's file empty and owner-only if it is not there, so a
    /// following in-place write or append finds it and keeps its mode. Returns the path if it was
    /// created here, so a failed write can take it away again.
    fn precreate(&self, key: &str, op: &str) -> Result<Option<PathBuf>, BlobError> {
        let Some(FsDirs { root, .. }) = self.fs.as_ref().filter(|_| self.owner_only) else {
            return Ok(None);
        };
        let target = key_path(root, key, op)?;
        let io = |what: &str, e: std::io::Error| BlobError::Backend(format!("{what}: {e}"));
        if let Some(parent) = target.parent() {
            persistant_core::owner_only::create_dir_all(parent)
                .map_err(|e| io("create the key's directory", e))?;
        }
        match persistant_core::owner_only::create_new(&target) {
            Ok(_) => Ok(Some(target)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(io("create the owner-only file", e)),
        }
    }

    /// The facts a matrix row for this backend is recorded under.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A streamed-write handle. Refused where OpenDAL would write the target in place (`fs`
    /// without `atomic_write_dir`): bytes would be visible before `finish` and there is no abort,
    /// so the contract cannot hold, and the answer is `Unsupported`, as for `replace_atomic`.
    async fn open_sink(&self, key: &str, op: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        if !self.atomic_replace {
            return Err(BlobError::Unsupported(format!(
                "{op} without atomic_write_dir writes in place"
            )));
        }
        if let Some(FsDirs {
            root,
            scratch: Some(scratch),
        }) = &self.fs
        {
            clean_staging(scratch);
            return HandSink::open(root, scratch, key, self.owner_only).await;
        }
        let writer = self.op.writer(key).await.map_err(backend)?;
        Ok(Box::new(OpendalSink {
            writer: Some(writer),
            memory: self.fs.is_none(),
        }))
    }
}

impl OpendalBlob {
    /// A whole value written through the hand-staged sink (an owner-only scratch file renamed onto
    /// the key): the replace is atomic, as a scratch directory promises.
    async fn write_staged(&self, key: &str, bytes: &[u8]) -> Result<(), BlobError> {
        let mut sink = self.open_sink(key, "write").await?;
        if let Err(e) = sink.write(bytes).await {
            let _ = sink.abort().await;
            return Err(e);
        }
        sink.finish().await
    }

    /// `create_exclusive` for an owner-only `fs` with no scratch directory: the file is created
    /// owner-only with `create_new` (exclusive), then filled. A reader may see it empty before the
    /// fill; that is what this configuration promises (no `blob.create_exclusive_atomic`).
    fn create_in_place(&self, key: &str, bytes: &[u8]) -> Result<bool, BlobError> {
        use std::io::Write;
        let Some(FsDirs { root, .. }) = &self.fs else {
            return Ok(false);
        };
        let target = key_path(root, key, "create_exclusive")?;
        let io = |what: &str, e: std::io::Error| BlobError::Backend(format!("{what}: {e}"));
        if let Some(parent) = target.parent() {
            persistant_core::owner_only::create_dir_all(parent)
                .map_err(|e| io("create the key's directory", e))?;
        }
        let mut f = match persistant_core::owner_only::create_new(&target) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(e) => return Err(io("create the owner-only file", e)),
        };
        if let Err(e) = f.write_all(bytes) {
            drop(f);
            let _ = std::fs::remove_file(&target);
            return Err(io("write the file", e));
        }
        Ok(true)
    }
}

/// The path of `key` under `root`, or an error if it is not a plain relative path.
fn key_path(root: &Path, key: &str, op: &str) -> Result<PathBuf, BlobError> {
    let rel = Path::new(key.trim_start_matches('/'));
    if rel.as_os_str().is_empty() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(BlobError::Backend(format!(
            "{op}: {key:?} is not a path under the root"
        )));
    }
    Ok(root.join(rel))
}

impl BlobBackend for OpendalBlob {
    fn backend(&self) -> &str {
        self.name
    }

    fn version(&self) -> &str {
        opendal::raw::VERSION
    }

    /// A key-value store on `fs` encodes its keys (`HexPath`): a key is a file path there, and a
    /// filesystem may ignore case, reserve names, or make a key another key's directory.
    fn key_layout(&self) -> KeyLayout {
        if self.fs.is_some() {
            KeyLayout::HexPath
        } else {
            KeyLayout::Raw
        }
    }

    async fn read(&self, key: &str) -> Result<Option<Vec<u8>>, BlobError> {
        match self.op.read(key).await {
            Ok(buf) => Ok(Some(buf.to_vec())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    async fn write(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        if self.owner_only && self.atomic_replace && self.fs.is_some() {
            // OpenDAL would rename a default-mode scratch file onto the key: stage it by hand.
            return self.write_staged(key, &bytes).await;
        }
        let created = self.precreate(key, "write")?;
        let r = self.op.write(key, bytes).await.map(|_| ()).map_err(backend);
        if r.is_err()
            && let Some(p) = created
        {
            let _ = std::fs::remove_file(p);
        }
        r
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
        if let Some(FsDirs {
            root,
            scratch: Some(scratch),
        }) = &self.fs
        {
            return create_by_hard_link(root, scratch, key, &bytes, self.owner_only).await;
        }
        if self.owner_only && self.fs.is_some() {
            return self.create_in_place(key, &bytes);
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
        if self.owner_only && self.fs.is_some() {
            return self.write_staged(key, &bytes).await;
        }
        self.op.write(key, bytes).await.map(|_| ()).map_err(backend)
    }

    async fn read_range(
        &self,
        key: &str,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>, BlobError> {
        // The length first: a range past the end is clamped here, not left to each service.
        let Some(e) = self.stat(key).await? else {
            return Ok(None);
        };
        let start = offset.min(e.len);
        let end = start.saturating_add(len).min(e.len);
        if start >= end {
            return Ok(Some(Vec::new()));
        }
        match self.op.read_with(key).range(start..end).await {
            Ok(buf) => Ok(Some(buf.to_vec())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(backend(e)),
        }
    }

    async fn write_stream(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        self.open_sink(key, "write_stream").await
    }

    async fn write_stream_atomic(&self, key: &str) -> Result<Box<dyn BlobSink>, BlobError> {
        self.open_sink(key, "write_stream_atomic").await
    }

    async fn append(&self, key: &str, bytes: Vec<u8>) -> Result<(), BlobError> {
        let created = self.precreate(key, "append")?;
        let r = match self.op.write_with(key, bytes).append(true).await {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == ErrorKind::Unsupported => {
                Err(BlobError::Unsupported(e.to_string()))
            }
            Err(e) => Err(backend(e)),
        };
        if r.is_err()
            && let Some(p) = created
        {
            let _ = std::fs::remove_file(p);
        }
        r
    }
}

/// A streamed write through OpenDAL's writer. With `atomic_write_dir` (or on the memory service)
/// the bytes are not at the key until `close`; `abort` discards them. Dropping an unfinished sink
/// aborts it on the current runtime if there is one, so the scratch file does not outlive it.
struct OpendalSink {
    writer: Option<opendal::Writer>,
    memory: bool,
}

impl BlobSink for OpendalSink {
    fn write<'a>(&'a mut self, chunk: &'a [u8]) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            let w = self
                .writer
                .as_mut()
                .ok_or_else(|| BlobError::Backend("the sink is finished".to_string()))?;
            w.write(chunk.to_vec()).await.map_err(backend)
        })
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            let mut w = self
                .writer
                .take()
                .ok_or_else(|| BlobError::Backend("the sink is finished".to_string()))?;
            w.close().await.map(|_| ()).map_err(backend)
        })
    }

    fn abort(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            let Some(mut w) = self.writer.take() else {
                return Ok(());
            };
            match w.abort().await {
                Ok(()) => Ok(()),
                // The memory service holds the bytes in the writer: dropping it discards them.
                Err(e) if self.memory && e.kind() == ErrorKind::Unsupported => Ok(()),
                Err(e) => Err(backend(e)),
            }
        })
    }
}

impl Drop for OpendalSink {
    fn drop(&mut self) {
        if let Some(mut w) = self.writer.take()
            && let Ok(rt) = tokio::runtime::Handle::try_current()
        {
            rt.spawn(async move {
                let _ = w.abort().await;
            });
        }
    }
}

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

const STAGING_SUFFIX: &str = ".part";

/// A streamed write on `fs` with a scratch directory, staged by hand (docs/DESIGN-BLOB-STREAMS.md
/// 3.3, 3.8): OpenDAL's writer names and owns its scratch file, so nothing could tell a dead
/// writer's file from a live one. This sink creates `<pid>-<seq>-<time>.part` in the scratch
/// directory, holds an exclusive advisory lock on it while it lives, and on `finish` syncs it and
/// renames it onto the key. The operating system drops the lock when the process dies, however it
/// dies, which is what [`clean_staging`] relies on.
struct HandSink {
    file: Option<tokio::fs::File>,
    /// The staging file; `None` once it has been renamed or removed.
    staged: Option<PathBuf>,
    target: PathBuf,
}

impl HandSink {
    async fn open(
        root: &Path,
        scratch: &Path,
        key: &str,
        owner_only: bool,
    ) -> Result<Box<dyn BlobSink>, BlobError> {
        let io = |what: &str, e: std::io::Error| BlobError::Backend(format!("{what}: {e}"));
        let target = key_path(root, key, "write_stream")?;
        let make_dir = |dir: &Path| {
            if owner_only {
                persistant_core::owner_only::create_dir_all(dir)
            } else {
                std::fs::create_dir_all(dir)
            }
        };
        if let Some(parent) = target.parent() {
            make_dir(parent).map_err(|e| io("create the key's directory", e))?;
        }
        make_dir(scratch).map_err(|e| io("create the scratch directory", e))?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let staged = scratch.join(format!(
            "{}-{}-{nanos:x}{STAGING_SUFFIX}",
            std::process::id(),
            SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let file = if owner_only {
            persistant_core::owner_only::create_new(&staged)
        } else {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&staged)
        }
        .map_err(|e| io("create the staging file", e))?;
        // Our own new name: the lock cannot be held by anyone else.
        if let Err(e) = file.try_lock() {
            drop(file);
            let _ = std::fs::remove_file(&staged);
            return Err(io("lock the staging file", std::io::Error::other(e)));
        }
        Ok(Box::new(HandSink {
            file: Some(tokio::fs::File::from_std(file)),
            staged: Some(staged),
            target,
        }))
    }

    /// Close the handle (releasing the lock), then remove the staging file if it is still there.
    fn discard(&mut self) {
        self.file = None;
        if let Some(p) = self.staged.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl BlobSink for HandSink {
    fn write<'a>(&'a mut self, chunk: &'a [u8]) -> BoxFuture<'a, Result<(), BlobError>> {
        Box::pin(async move {
            use tokio::io::AsyncWriteExt;
            let f = self
                .file
                .as_mut()
                .ok_or_else(|| BlobError::Backend("the sink is finished".to_string()))?;
            f.write_all(chunk)
                .await
                .map_err(|e| BlobError::Backend(format!("write the staging file: {e}")))
        })
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            let io = |what: &str, e: std::io::Error| BlobError::Backend(format!("{what}: {e}"));
            let (Some(f), Some(staged)) = (self.file.as_ref(), self.staged.clone()) else {
                return Err(BlobError::Backend("the sink is finished".to_string()));
            };
            if let Err(e) = f.sync_all().await {
                self.discard();
                return Err(io("sync the staging file", e));
            }
            // The lock is released first. On Windows `LockFileEx` is mandatory, not advisory: a file
            // renamed onto the key with the lock still held would refuse every reader (os error 33,
            // seen on CI) until the handle closed. The price is a window between this unlock and the
            // rename in which a cleanup in another process may remove the staging file; the rename
            // then fails, `finish` returns an error, and the old value is untouched.
            if let Some(file) = self.file.take() {
                let std_file = file.into_std().await;
                if let Err(e) = std_file.unlock() {
                    drop(std_file);
                    self.discard();
                    return Err(io("release the staging lock", e));
                }
            }
            #[cfg(feature = "failpoint")]
            failpoint_pause_before_publish();
            match rename_retrying(staged, self.target.clone()).await {
                Ok(()) => {
                    self.staged = None;
                    self.file = None;
                    Ok(())
                }
                Err(e) => {
                    self.discard();
                    Err(io("rename the staging file onto the key", e))
                }
            }
        })
    }

    fn abort(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BlobError>> {
        Box::pin(async move {
            self.discard();
            Ok(())
        })
    }
}

impl Drop for HandSink {
    /// A dropped, unfinished sink removes its own staging; no runtime is needed.
    fn drop(&mut self) {
        self.discard();
    }
}

/// Test-only failpoint (feature `failpoint`, off by default): when `PERSISTANT_FAILPOINT_FINISH` is
/// set, `finish` prints `paused` and blocks here, staging complete and unpublished, so a test can kill
/// the process at exactly this point (docs/DESIGN-BLOB-STREAMS.md section 4, crash test 4b).
#[cfg(feature = "failpoint")]
fn failpoint_pause_before_publish() {
    if std::env::var_os("PERSISTANT_FAILPOINT_FINISH").is_some() {
        println!("paused");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
}

/// `rename`, retried for a short while on `PermissionDenied`. On Windows a rename onto a key a
/// reader has open (or that an indexer or scanner touches at that instant) can fail with a sharing
/// violation that clears within milliseconds; any other error, and the last one, is returned.
async fn rename_retrying(from: PathBuf, to: PathBuf) -> std::io::Result<()> {
    tokio::task::spawn_blocking(move || {
        let mut attempt = 0;
        loop {
            match std::fs::rename(&from, &to) {
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempt < 50 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                other => return other,
            }
        }
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Remove staging files in `scratch` whose writer is gone: a `.part` file is removed only if its
/// lock can be taken, so a live sink in this or another process keeps its file. Errors are left
/// alone (the directory may not exist yet; a file may belong to a writer that is finishing).
fn clean_staging(scratch: &Path) {
    let Ok(dir) = std::fs::read_dir(scratch) else {
        return;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(&STAGING_SUFFIX[1..]) {
            continue;
        }
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        else {
            continue;
        };
        if file.try_lock().is_ok() {
            drop(file);
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Exclusive create for `fs` with a scratch directory: write the whole value to a new file in the
/// scratch directory, then `hard_link` it to the target. The link is atomic and fails if the target
/// exists, so exactly one of several creators succeeds, and the value is complete the moment the
/// key exists. The scratch directory must be on the same filesystem as the root and the filesystem
/// must support hard links; where it does not, the answer is an error, never a silent fallback to
/// check-then-write, so the conformance run records the leaf off.
async fn create_by_hard_link(
    root: &Path,
    scratch: &Path,
    key: &str,
    bytes: &[u8],
    owner_only: bool,
) -> Result<bool, BlobError> {
    use tokio::io::AsyncWriteExt;
    let io = |what: &str, e: std::io::Error| BlobError::Backend(format!("{what}: {e}"));
    let target = key_path(root, key, "create_exclusive")?;
    let make_dir = |dir: &Path| {
        if owner_only {
            persistant_core::owner_only::create_dir_all(dir)
        } else {
            std::fs::create_dir_all(dir)
        }
    };
    if let Some(parent) = target.parent() {
        make_dir(parent).map_err(|e| io("create the key's directory", e))?;
    }
    make_dir(scratch).map_err(|e| io("create the scratch directory", e))?;
    let temp = scratch.join(format!(
        ".persistant-create-{}-{}",
        std::process::id(),
        SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let written = async {
        let mut f = if owner_only {
            tokio::fs::File::from_std(persistant_core::owner_only::create_new(&temp)?)
        } else {
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .await?
        };
        f.write_all(bytes).await?;
        f.sync_all().await
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(io("write the scratch file", e));
    }
    let linked = tokio::fs::hard_link(&temp, &target).await;
    let _ = tokio::fs::remove_file(&temp).await;
    match linked {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => Err(BlobError::Unsupported(
            format!("create_exclusive needs hard links on the root's filesystem: {e}"),
        )),
        Err(e) => Err(io("link the scratch file to the key", e)),
    }
}
