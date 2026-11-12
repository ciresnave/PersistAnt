// SPDX-License-Identifier: MIT OR Apache-2.0
//! `lock.process`: a lock held until the holding process dies, with no staleness.
//!
//! An operating-system advisory lock on a file (`std::fs::File::try_lock`: `flock` on Unix,
//! `LockFileEx` on Windows). Dropping it unlocks explicitly (a forked child can hold a copy of the file
//! description until it `exec`s, which keeps a `flock` held after a plain close). The operating system releases it when the process ends, however it
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

impl Drop for ProcessLock {
    /// Release the lock now, explicitly. Closing the descriptor is not enough: a `flock` belongs to the
    /// open file *description*, and a child forked while the lock was held keeps a copy of that
    /// description until it `exec`s, so a close alone left the lock held for that moment (issue #220;
    /// close-on-exec does not help before the `exec`). `unlock` acts on the description, whoever else
    /// still holds a descriptor to it. An error is ignored: the descriptor closes right after, and the
    /// operating system releases the lock when the process ends in any case.
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// The environment variable that makes a process the lock-test child: `lock|<path>` holds a
/// [`ProcessLock`] on `<path>`, `marker|<path>` creates the known-bad marker file there.
pub const CHILD_ENV: &str = "PERSISTANT_LOCK_CHILD";

/// If this process was started as the `lock.process` child, hold what it was told to hold and wait
/// to be killed (it never returns). Otherwise return at once.
pub fn maybe_run_child() {
    use std::io::Write;
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let Some((mode, path)) = spec.split_once('|') else {
        panic!("bad {CHILD_ENV}: {spec}");
    };
    let path = Path::new(path);
    let _held = match mode {
        "lock" => Some(
            ProcessLock::try_acquire(path)
                .expect("the child can open the lock file")
                .expect("the child finds the lock free"),
        ),
        "marker" => {
            // The known-bad "lock": exclusive create of a marker, removed only by a clean exit.
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .expect("the child creates the marker");
            None
        }
        other => panic!("unknown lock child mode {other}"),
    };
    println!("held");
    let _ = std::io::stdout().flush();
    std::thread::sleep(std::time::Duration::from_secs(120));
    std::process::exit(0);
}

fn spawn_holder(mode: &str, path: &Path) -> io::Result<std::process::Child> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let mut child = Command::new(std::env::current_exe()?)
        .env(CHILD_ENV, format!("{mode}|{}", path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut out = BufReader::new(child.stdout.take().expect("piped"));
    let mut line = String::new();
    loop {
        line.clear();
        if out.read_line(&mut line)? == 0 {
            return Err(io::Error::other(
                "the lock-test child exited before reporting it held the lock",
            ));
        }
        if line.trim() == "held" {
            return Ok(child);
        }
    }
}

/// Decide `lock.process` for the operating-system lock, as a matrix row.
///
/// The target: a child process locks a file; while it lives the parent is refused; the child is
/// killed hard; the parent is then granted the lock. Controls: the observation the verdict rests on
/// (a live child's lock is seen as held) and the known-bad "lock" (a marker file, which is still
/// there after the same kill). The host's own executable must call [`maybe_run_child`] first in
/// `main`, because the child is that executable.
pub fn certify(work: &Path, env: &persistant_core::Config) -> io::Result<persistant_core::Row> {
    use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
    let lock = work.join("the.lock");
    let marker = work.join("the.marker");

    let mut holder = spawn_holder("lock", &lock)?;
    let refused_while_alive = ProcessLock::try_acquire(&lock)?.is_none();
    holder.kill()?; // hard stop: no destructors, no cleanup
    holder.wait()?;
    let free_after_kill = ProcessLock::try_acquire(&lock)?.is_some();

    let mut bad = spawn_holder("marker", &marker)?;
    let marker_while_alive = marker.exists();
    bad.kill()?;
    bad.wait()?;
    let marker_survives = marker.exists();

    let controls = Controls {
        positive_passed: refused_while_alive && marker_while_alive,
        known_bad_failed: marker_survives,
    };
    let verdict = if refused_while_alive && free_after_kill {
        Verdict::Exact
    } else {
        Verdict::Wrong
    };
    let id = CapabilityId::new("lock.process").expect("valid id");
    let metadata = env
        .clone()
        .with("test", "process_lock::lock.process")
        .with("kill", "hard");
    let config = Config::new();
    Ok(
        match verdict.into_row(
            id.clone(),
            "std-file-lock",
            "File::try_lock",
            config.clone(),
            metadata.clone(),
            controls,
        ) {
            Ok(row) => row,
            Err(_) => Row {
                subject_kind: "capability".to_string(),
                subject: id,
                backend: "std-file-lock".to_string(),
                version: "File::try_lock".to_string(),
                config,
                enabled: false,
                reason: Some(Reason::Untested),
                metadata,
                note: format!("controls did not hold: {controls:?}"),
                extra: Default::default(),
            },
        },
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Issue #220. A `flock` belongs to the open file *description*, so a child forked while the lock is
    /// held keeps it held, after the owner drops, until the child `exec`s. A duplicated descriptor
    /// stands in for that child's copy: dropping the lock must release it all the same.
    #[test]
    fn drop_releases_while_another_descriptor_of_the_same_lock_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("the.lock");
        let first = ProcessLock::try_acquire(&path)
            .unwrap()
            .expect("free lock is granted");
        let copy = first._file.try_clone().unwrap();
        drop(first);
        assert!(
            ProcessLock::try_acquire(&path).unwrap().is_some(),
            "a dropped lock was still held by another descriptor of the same open file"
        );
        drop(copy);
    }

    /// std opens with close-on-exec, so an `exec`ed child never keeps the lock file: the part of the
    /// inheritance story `O_CLOEXEC` does cover (the part before `exec` is the test above).
    #[cfg(target_os = "linux")]
    #[test]
    fn the_lock_descriptor_is_close_on_exec() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("the.lock");
        let lock = ProcessLock::try_acquire(&path)
            .unwrap()
            .expect("free lock is granted");
        let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", lock._file.as_raw_fd()))
            .unwrap();
        let flags = info
            .lines()
            .find_map(|l| l.strip_prefix("flags:"))
            .map(|v| u32::from_str_radix(v.trim(), 8).unwrap())
            .expect("fdinfo has a flags line");
        assert!(
            flags & 0o2_000_000 != 0,
            "O_CLOEXEC is not set: flags {flags:o}"
        );
    }
}
