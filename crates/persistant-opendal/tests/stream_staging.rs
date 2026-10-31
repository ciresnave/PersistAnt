// SPDX-License-Identifier: MIT OR Apache-2.0
//! Staging of streamed writes across processes (docs/DESIGN-BLOB-STREAMS.md 3.8, work order 3).
//!
//! Two things must both hold on `fs` with a scratch directory: staging whose writer was killed is
//! removed when a store is opened, and staging whose writer is alive (in another process) is not.
//! The children are this same test binary re-run with an environment variable, as in
//! `process_lock.rs`; `child_*` tests are no-ops unless their variable is set.

use persistant_blob::BlobBackend;
use persistant_opendal::OpendalBlob;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const CHILD_STREAM: &str = "PERSISTANT_TEST_STREAM_ROOT";
const CHILD_OPEN: &str = "PERSISTANT_TEST_OPEN_ROOT";
const CHILD_NAIVE: &str = "PERSISTANT_TEST_NAIVE_CLEANUP";
const SCRATCH_VAR: &str = "PERSISTANT_TEST_SCRATCH";

fn report(word: &str) {
    println!("{word}");
    let _ = std::io::stdout().flush();
}

fn scratch_entries(scratch: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(scratch)
        .map(|d| d.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default()
}

/// Child: open a store, start a stream to `k`, write one chunk, report `held`, wait to be killed.
#[tokio::test]
async fn child_stream_and_hold() {
    let Ok(root) = std::env::var(CHILD_STREAM) else {
        return;
    };
    let scratch = std::env::var(SCRATCH_VAR).unwrap();
    let b = OpendalBlob::fs(Path::new(&root), Some(Path::new(&scratch))).unwrap();
    let mut sink = b.write_stream("k").await.unwrap();
    sink.write(&vec![7u8; 1 << 20]).await.unwrap();
    report("held");
    std::thread::sleep(Duration::from_secs(120));
}

/// Child: open a store on the same directories (which is when cleanup runs), then exit.
/// With the naive-cleanup variable set it instead deletes every scratch file, the known-bad control.
#[tokio::test]
async fn child_open_and_exit() {
    let Ok(root) = std::env::var(CHILD_OPEN) else {
        return;
    };
    let scratch = std::env::var(SCRATCH_VAR).unwrap();
    if std::env::var(CHILD_NAIVE).is_ok() {
        for p in scratch_entries(Path::new(&scratch)) {
            let _ = std::fs::remove_file(p);
        }
    } else {
        let _b = OpendalBlob::fs(Path::new(&root), Some(Path::new(&scratch))).unwrap();
    }
    report("opened");
}

fn spawn_child(test: &str, vars: &[(&str, &Path)], flag: Option<&str>) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    if let Some(f) = flag {
        cmd.env(f, "1");
    }
    cmd.spawn().unwrap()
}

/// Read the child's stdout until it reports `word` (libtest prefixes the line under --nocapture).
fn wait_for(child: &mut Child, word: &str) {
    let mut out = BufReader::new(child.stdout.as_mut().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the child exited before reporting {word}"
        );
        if line.trim().ends_with(word) {
            return;
        }
    }
}

#[tokio::test]
async fn staging_left_by_a_killed_writer_is_removed_when_a_store_is_opened() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut child = spawn_child(
        "child_stream_and_hold",
        &[(CHILD_STREAM, root.path()), (SCRATCH_VAR, scratch.path())],
        None,
    );
    wait_for(&mut child, "held");
    child.kill().unwrap(); // hard stop: no destructors, no abort
    child.wait().unwrap();
    // Control: the kill really left staging behind, so the assertion below asks the question.
    assert!(
        !scratch_entries(scratch.path()).is_empty(),
        "control broken: a killed writer left nothing in the scratch directory"
    );
    assert_eq!(
        OpendalBlob::fs(root.path(), Some(scratch.path()))
            .unwrap()
            .read("k")
            .await
            .unwrap(),
        None,
        "an unfinished stream showed up at the key"
    );
    assert!(
        scratch_entries(scratch.path()).is_empty(),
        "opening the store left the dead writer's staging: {:?}",
        scratch_entries(scratch.path())
    );
}

/// Crash test 4a with an existing key: the killed writer's stream never shows at `k`, the whole old
/// value stays, and opening the store removes the staging.
#[tokio::test]
async fn a_killed_writer_leaves_the_whole_old_value_of_an_existing_key() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let old = vec![9u8; 3 << 20];
    OpendalBlob::fs(root.path(), Some(scratch.path()))
        .unwrap()
        .write("k", old.clone())
        .await
        .unwrap();
    let mut child = spawn_child(
        "child_stream_and_hold",
        &[(CHILD_STREAM, root.path()), (SCRATCH_VAR, scratch.path())],
        None,
    );
    wait_for(&mut child, "held");
    child.kill().unwrap();
    child.wait().unwrap();
    // Control: the kill left staging behind, so the reads below ask the question.
    assert!(
        !scratch_entries(scratch.path()).is_empty(),
        "control broken: a killed writer left nothing in the scratch directory"
    );
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    assert_eq!(
        b.read("k").await.unwrap().as_deref(),
        Some(&old[..]),
        "the old value is not whole after the writer was killed"
    );
    assert!(
        scratch_entries(scratch.path()).is_empty(),
        "opening the store left the dead writer's staging: {:?}",
        scratch_entries(scratch.path())
    );
}

/// Parent holds a live sink mid-stream; a second process opens the same directories; the sink must
/// still finish with the whole value. Returns the value read back after `finish`.
async fn live_sink_while_another_process_opens(naive: bool) -> (Option<Vec<u8>>, bool) {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let mut sink = b.write_stream("k").await.unwrap();
    sink.write(&vec![1u8; 1 << 20]).await.unwrap();
    let mut child = spawn_child(
        "child_open_and_exit",
        &[(CHILD_OPEN, root.path()), (SCRATCH_VAR, scratch.path())],
        naive.then_some(CHILD_NAIVE),
    );
    wait_for(&mut child, "opened");
    child.wait().unwrap();
    let _ = sink.write(&vec![2u8; 1 << 20]).await;
    let finished = sink.finish().await.is_ok();
    (b.read("k").await.unwrap(), finished)
}

#[tokio::test]
async fn staging_held_by_a_live_writer_in_another_process_is_left_alone() {
    let (value, finished) = live_sink_while_another_process_opens(false).await;
    let mut whole = vec![1u8; 1 << 20];
    whole.extend(vec![2u8; 1 << 20]);
    assert!(
        finished,
        "finish failed after another process opened the store"
    );
    assert_eq!(
        value.as_deref(),
        Some(&whole[..]),
        "the value is not the whole stream"
    );
}

/// Known-bad control: a cleanup that removes every scratch file breaks the live sink. Unix only:
/// Windows refuses to delete a file another handle holds open, so the control cannot bite there.
#[cfg(unix)]
#[tokio::test]
async fn known_bad_a_cleanup_that_removes_every_staging_file_breaks_a_live_writer() {
    let (value, finished) = live_sink_while_another_process_opens(true).await;
    let mut whole = vec![1u8; 1 << 20];
    whole.extend(vec![2u8; 1 << 20]);
    assert!(
        !finished || value.as_deref() != Some(&whole[..]),
        "control broken: removing every staging file did not break the live writer"
    );
}
