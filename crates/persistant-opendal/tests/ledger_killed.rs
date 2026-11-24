// SPDX-License-Identifier: MIT OR Apache-2.0
//! ROADMAP row 19, `Ledger`: a migrator **process** killed inside a step leaves a claim and no done
//! marker; the next run reports it by name (`StepOutcome::Claimed`) and does not run the step again.
//! Only the operator removing the `.claim` lets the step run. The control is the same child left to
//! finish: it records the step done, so the kill (not the child's code) is what leaves the claim.
//! Children are this test binary re-run with an environment variable, as in `crash_finish.rs`.

use persistant_blob::BlobBackend;
use persistant_blob::ledger::{Ledger, StepOutcome};
use persistant_opendal::OpendalBlob;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const ROOT_VAR: &str = "PERSISTANT_TEST_LEDGER_ROOT";
const HOLD_VAR: &str = "PERSISTANT_TEST_LEDGER_HOLD";
const PREFIX: &str = "migrations/";

fn report(word: &str) {
    println!("{word}");
    let _ = std::io::stdout().flush();
}

/// Child: claim step `s1`, announce it is inside the step, then hold (if told to) or finish.
#[tokio::test]
async fn child_migrator() {
    let Ok(root) = std::env::var(ROOT_VAR) else {
        return;
    };
    let hold = std::env::var(HOLD_VAR).is_ok();
    let b = OpendalBlob::fs(Path::new(&root), None).unwrap();
    let ledger = Ledger::new(&b, PREFIX);
    let out = ledger
        .run("s1", "child-pid", "t0", || async {
            report("in-step");
            if hold {
                std::thread::sleep(Duration::from_secs(120));
            }
            Ok::<(), ()>(())
        })
        .await;
    assert!(matches!(out, Ok(StepOutcome::Ran)));
}

fn spawn_child(root: &Path, hold: bool) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.args([
        "child_migrator",
        "--exact",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(ROOT_VAR, root)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if hold {
        cmd.env(HOLD_VAR, "1");
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

#[tokio::test]
async fn a_migrator_killed_inside_a_step_leaves_a_claim_that_is_reported_not_taken_over() {
    let root = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), None).unwrap();
    let mut child = spawn_child(root.path(), true);
    wait_for(&mut child, "in-step");
    child.kill().unwrap(); // hard stop: no destructors, no release of the claim
    child.wait().unwrap();

    let ledger = Ledger::new(&b, PREFIX);
    let reran = std::sync::atomic::AtomicBool::new(false);
    let out = ledger
        .run("s1", "operator", "t1", || async {
            reran.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<(), ()>(())
        })
        .await
        .unwrap();
    assert_eq!(out, StepOutcome::Claimed("t0 child-pid".to_string()));
    assert!(!reran.load(std::sync::atomic::Ordering::SeqCst));
    assert!(b.read("migrations/s1.done").await.unwrap().is_none());

    // The operator removes the claim after checking the store; then the step runs and is recorded.
    b.delete("migrations/s1.claim").await.unwrap();
    let out = ledger
        .run("s1", "operator", "t2", || async { Ok::<(), ()>(()) })
        .await
        .unwrap();
    assert_eq!(out, StepOutcome::Ran);
}

#[tokio::test]
async fn control_the_same_child_left_to_finish_records_the_step_done() {
    let root = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), None).unwrap();
    let mut child = spawn_child(root.path(), false);
    wait_for(&mut child, "in-step");
    assert!(child.wait().unwrap().success());
    let out = Ledger::new(&b, PREFIX)
        .run("s1", "operator", "t1", || async { Ok::<(), ()>(()) })
        .await
        .unwrap();
    assert_eq!(out, StepOutcome::AlreadyDone);
}
