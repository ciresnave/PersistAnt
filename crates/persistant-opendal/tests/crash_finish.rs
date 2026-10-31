// SPDX-License-Identifier: MIT OR Apache-2.0
//! Crash test 4b (docs/DESIGN-BLOB-STREAMS.md section 4): a process killed **during** the publish
//! step of `write_stream_atomic` leaves the whole old value, never a mix, and the key stays usable.
//! The pause is the `failpoint` feature's, inside `finish`. The known-bad control is a writer that
//! opens the key itself and writes into it, bypassing scratch and rename; the same kill must leave
//! it partial, or the assertion on the real writer proves nothing.
//! Children are this test binary re-run with an environment variable, as in `stream_staging.rs`.

use persistant_blob::BlobBackend;
use persistant_opendal::OpendalBlob;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const CHILD_ATOMIC: &str = "PERSISTANT_TEST_FINISH_ROOT";
const CHILD_IN_PLACE: &str = "PERSISTANT_TEST_INPLACE_ROOT";
const SCRATCH_VAR: &str = "PERSISTANT_TEST_SCRATCH";
const HALF: usize = 1 << 20;

fn old_value() -> Vec<u8> {
    vec![1u8; 2 * HALF]
}

fn new_value() -> Vec<u8> {
    vec![2u8; 2 * HALF]
}

fn report(word: &str) {
    println!("{word}");
    let _ = std::io::stdout().flush();
}

/// Child: stream the whole new value to `k` and call `finish`, which blocks at the failpoint.
#[tokio::test]
async fn child_finish_and_pause() {
    let Ok(root) = std::env::var(CHILD_ATOMIC) else {
        return;
    };
    let scratch = std::env::var(SCRATCH_VAR).unwrap();
    let b = OpendalBlob::fs(Path::new(&root), Some(Path::new(&scratch))).unwrap();
    let mut sink = b.write_stream_atomic("k").await.unwrap();
    sink.write(&new_value()[..HALF]).await.unwrap();
    sink.write(&new_value()[HALF..]).await.unwrap();
    let _ = sink.finish().await; // never returns before the kill
}

/// Child, the known-bad control: write half the new value straight into the key, then wait.
#[test]
fn child_in_place_and_hold() {
    let Ok(root) = std::env::var(CHILD_IN_PLACE) else {
        return;
    };
    let mut f = std::fs::File::create(Path::new(&root).join("k")).unwrap();
    f.write_all(&new_value()[..HALF]).unwrap();
    f.sync_all().unwrap();
    report("half");
    std::thread::sleep(Duration::from_secs(120));
}

fn spawn_child(test: &str, vars: &[(&str, &Path)], failpoint: bool) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in vars {
        cmd.env(k, v);
    }
    if failpoint {
        cmd.env("PERSISTANT_FAILPOINT_FINISH", "1");
    }
    cmd.spawn().unwrap()
}

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

fn kill(mut child: Child) {
    child.kill().unwrap(); // hard stop: no destructors
    child.wait().unwrap();
}

#[tokio::test]
async fn a_writer_killed_during_finish_leaves_the_whole_old_value_and_the_key_stays_usable() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    b.write("k", old_value()).await.unwrap();
    let mut child = spawn_child(
        "child_finish_and_pause",
        &[(CHILD_ATOMIC, root.path()), (SCRATCH_VAR, scratch.path())],
        true,
    );
    wait_for(&mut child, "paused");
    kill(child);
    // Control: the kill landed inside finish, with the staged copy complete and unpublished.
    let staged: Vec<_> = std::fs::read_dir(scratch.path()).unwrap().collect();
    assert_eq!(staged.len(), 1, "control broken: no staging at the pause");
    assert_eq!(
        std::fs::metadata(staged[0].as_ref().unwrap().path())
            .unwrap()
            .len(),
        new_value().len() as u64,
        "control broken: the staged copy was not complete at the pause"
    );
    assert_eq!(
        b.read("k").await.unwrap(),
        Some(old_value()),
        "a kill during finish left something other than the whole old value"
    );
    // Reopen (cleanup runs), then the key takes a new atomic stream.
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let mut sink = b.write_stream_atomic("k").await.unwrap();
    sink.write(&new_value()).await.unwrap();
    sink.finish().await.unwrap();
    assert_eq!(b.read("k").await.unwrap(), Some(new_value()));
}

#[tokio::test]
async fn known_bad_an_in_place_writer_killed_midway_leaves_a_partial_value() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    b.write("k", old_value()).await.unwrap();
    let mut child = spawn_child(
        "child_in_place_and_hold",
        &[(CHILD_IN_PLACE, root.path())],
        false,
    );
    wait_for(&mut child, "half");
    kill(child);
    let got = b.read("k").await.unwrap().expect("the key vanished");
    assert!(
        got != old_value() && got != new_value(),
        "control broken: the in-place writer left a whole value ({} bytes)",
        got.len()
    );
}
