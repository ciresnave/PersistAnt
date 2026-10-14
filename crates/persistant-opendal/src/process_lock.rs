// SPDX-License-Identifier: MIT OR Apache-2.0
//! `lock.process`: a lock held until the holding process dies, with no staleness.
//!
//! An operating-system advisory lock on a file (`std::fs::File::try_lock`: `flock` on Unix,
//! `LockFileEx` on Windows). The operating system releases it when the process ends, however it
//! ends, which is what a marker file cannot do. It is **local only**: it says nothing across a
//! network filesystem, and it is advisory (a process that does not ask is not stopped).
//!
//! It uses only `std`; it lives in this crate because it is filesystem-bound, not because it uses
//! OpenDAL. The test that decides the capability starts a child process, locks in the child, kills
//! the child hard and checks the lock is free (`tests/process_lock.rs`); a library cannot run that
//! itself because the child must be the host's own executable.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;

/// A held process lock. Dropping it releases it; so does the process ending.
#[derive(Debug)]
pub struct ProcessLock {
    _file: File,
}

impl ProcessLock {
    /// Try to take the exclusive lock on `path`, creating the file if it is absent.
    /// `Ok(None)`: another handle (in this or another process) holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<ProcessLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(ProcessLock { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }
}
