// SPDX-License-Identifier: MIT OR Apache-2.0
//! The crash half of `blob.replace_atomic`, with controls, against the real fs service.
//!
//! This test binary is its own child: `child_entry` becomes the replacing process when
//! `PERSISTANT_CRASH_CHILD` is set, and does nothing otherwise.

use persistant_core::Config;
use persistant_opendal::crash::{self, Mode};

const CHILD_ARGS: [&str; 4] = ["child_entry", "--exact", "--nocapture", "--test-threads=1"];

#[test]
fn child_entry() {
    crash::maybe_run_child();
}

fn dirs(name: &str) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let root = work.path().join(format!("{name}-root"));
    let scratch = work.path().join(format!("{name}-scratch"));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&scratch).unwrap();
    (work, root, scratch)
}

#[test]
fn a_correct_replace_survives_every_kill() {
    let (_w, root, scratch) = dirs("opendal");
    let r = crash::run(Mode::Opendal, &root, &scratch, &CHILD_ARGS).unwrap();
    assert_eq!(r.rounds, 12);
    assert_eq!(r.torn, 0, "first torn state: {:?}", r.first_torn);
}

#[test]
fn positive_control_a_rename_replace_survives_every_kill() {
    let (_w, root, scratch) = dirs("rename");
    let r = crash::run(Mode::Rename, &root, &scratch, &CHILD_ARGS).unwrap();
    assert_eq!(r.torn, 0, "first torn state: {:?}", r.first_torn);
}

#[test]
fn known_bad_control_an_in_place_write_is_found_torn() {
    let (_w, root, scratch) = dirs("inplace");
    let r = crash::run(Mode::InPlace, &root, &scratch, &CHILD_ARGS).unwrap();
    assert!(
        r.torn > 0,
        "the in-place writer was never found torn in {} kills: the test cannot tell",
        r.rounds
    );
}

#[test]
fn certification_rows_say_on_with_the_scratch_dir_and_missing_without() {
    let work = tempfile::tempdir().unwrap();
    let rows = crash::certify_replace(work.path(), &CHILD_ARGS, &Config::new()).unwrap();
    assert_eq!(rows.len(), 2);
    let unset = &rows[0];
    let set = &rows[1];
    assert_eq!(unset.config.get("atomic_write_dir"), Some("unset"));
    assert!(!unset.enabled, "{}", unset.note);
    assert_eq!(set.config.get("atomic_write_dir"), Some("set"));
    assert!(set.enabled, "{}", set.note);
    assert_eq!(set.metadata.get("rounds"), Some("12"));
}
