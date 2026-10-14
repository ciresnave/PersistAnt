// SPDX-License-Identifier: MIT OR Apache-2.0
//! The test that decides `lock.process`: a lock held by a child process is held while the child
//! lives and free the moment it is killed, and a marker file (the known-bad control) is not.
//!
//! The child is this same test binary, re-run with an environment variable that makes one test
//! hold a lock and wait. `child_*` tests are no-ops unless that variable is set.

use persistant_opendal::ProcessLock;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const CHILD_LOCK: &str = "PERSISTANT_TEST_CHILD_LOCK";
const CHILD_MARKER: &str = "PERSISTANT_TEST_CHILD_MARKER";

fn hold_forever() {
    println!("held");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::thread::sleep(Duration::from_secs(120));
}

#[test]
fn child_hold_lock() {
    if let Ok(p) = std::env::var(CHILD_LOCK) {
        let _lock = ProcessLock::try_acquire(Path::new(&p)).unwrap().unwrap();
        hold_forever();
    }
}

#[test]
fn child_hold_marker() {
    if let Ok(p) = std::env::var(CHILD_MARKER) {
        // The known-bad "lock": exclusive create of a marker file, removed only on clean exit.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .unwrap();
        hold_forever();
    }
}

/// Start this binary running one `child_*` test, wait until it reports `held`, return the child.
fn spawn_holder(test: &str, var: &str, path: &Path) -> std::process::Child {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(var, path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        let n = out.read_line(&mut line).unwrap();
        assert!(n > 0, "the child exited before reporting it held the lock");
        // libtest prefixes the line with "test <name> ... " under --nocapture.
        if line.trim().ends_with("held") {
            break;
        }
    }
    child
}

fn lock_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("the.lock")
}

#[test]
fn a_second_handle_in_the_same_process_is_refused_and_drop_releases() {
    let dir = tempfile::tempdir().unwrap();
    let p = lock_path(&dir);
    let first = ProcessLock::try_acquire(&p)
        .unwrap()
        .expect("free lock is granted");
    assert!(
        ProcessLock::try_acquire(&p).unwrap().is_none(),
        "a held lock was granted twice"
    );
    drop(first);
    assert!(
        ProcessLock::try_acquire(&p).unwrap().is_some(),
        "a dropped lock was not released"
    );
}

#[test]
fn a_lock_held_by_another_process_is_free_when_that_process_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let p = lock_path(&dir);
    let mut child = spawn_holder("child_hold_lock", CHILD_LOCK, &p);
    assert!(
        ProcessLock::try_acquire(&p).unwrap().is_none(),
        "a lock held by a live child process was granted"
    );
    child.kill().unwrap(); // hard stop: no destructors, no cleanup
    child.wait().unwrap();
    assert!(
        ProcessLock::try_acquire(&p).unwrap().is_some(),
        "the operating system did not release the lock when its holder died"
    );
}

#[test]
fn known_bad_a_marker_file_is_still_held_after_its_holder_is_killed() {
    // Control: the same kill that frees a real lock leaves a marker file behind, so the test above
    // can tell a lock that dies with its process from one that does not.
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let mut child = spawn_holder("child_hold_marker", CHILD_MARKER, &marker);
    assert!(marker.exists());
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        marker.exists(),
        "control broken: a killed process cleaned up its marker file"
    );
    let second = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker);
    assert!(
        second.is_err(),
        "the stale marker still excludes every later holder"
    );
}
