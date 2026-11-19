// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/opendal.tsv` is readable by the gate's own reader, and says what the
//! findings in `conformance.rs` and `kv_on_fs.rs` say. This is the consumer's path: parse the
//! file, ask a question by `(subject, backend, version, config)`.
#![cfg(feature = "registry")]

use persistant::blob::BlobBackend;
use persistant::capability::{CapabilityId, Config, Lookup, Matrix, Reason};
use persistant::opendal_driver::OpendalBlob;

fn matrix() -> Matrix {
    let text = include_str!("../matrix/opendal.tsv");
    Matrix::parse(text).expect("the checked-in matrix parses")
}

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

#[test]
fn the_gate_reads_the_recorded_findings() {
    let m = matrix();
    let version = OpendalBlob::memory().unwrap().version().to_string();
    let unset = Config::new().with("atomic_write_dir", "unset");
    let set = Config::new().with("atomic_write_dir", "set");

    // fs without a scratch dir creates exclusively and does not replace atomically ...
    assert_eq!(
        m.lookup(&id("blob.create_exclusive"), "opendal-fs", &version, &unset),
        Lookup::On
    );
    assert_eq!(
        m.lookup(
            &id("blob.replace_atomic.concurrent_read"),
            "opendal-fs",
            &version,
            &unset
        ),
        Lookup::Off(Reason::Missing)
    );
    // ... and with one it replaces atomically and also creates exclusively: OpenDAL's own create
    // there is check-then-rename (README 0.4.7), so the driver links a finished scratch file to the
    // key instead (0.9.41, crates/persistant-opendal/tests/fs_create_exclusive.rs).
    assert_eq!(
        m.lookup(&id("blob.create_exclusive"), "opendal-fs", &version, &set),
        Lookup::On
    );
    assert_eq!(
        m.lookup(
            &id("blob.replace_atomic.concurrent_read"),
            "opendal-fs",
            &version,
            &set
        ),
        Lookup::On
    );
    // The crash half is certified too: atomic with a scratch dir, refused by the driver without.
    assert_eq!(
        m.lookup(&id("blob.replace_atomic"), "opendal-fs", &version, &set),
        Lookup::On
    );
    assert_eq!(
        m.lookup(&id("blob.replace_atomic"), "opendal-fs", &version, &unset),
        Lookup::Off(Reason::Missing)
    );
    // A server or driver version nobody certified is off, not guessed.
    assert_eq!(
        m.lookup(
            &id("blob.read"),
            "opendal-fs",
            "0.0.0-not-certified",
            &unset
        ),
        Lookup::NoRow
    );
    // A leaf nobody wrote a test for has no row at all.
    assert_eq!(
        m.lookup(&id("blob.durable"), "opendal-fs", &version, &unset),
        Lookup::NoRow
    );
}

#[test]
fn every_row_carries_its_test_and_environment() {
    let m = matrix();
    assert!(!m.rows().is_empty());
    for r in m.rows() {
        assert!(r.metadata.get("test").is_some(), "{}", r.subject);
        assert!(r.metadata.get("os").is_some(), "{}", r.subject);
        assert_eq!(r.enabled, r.reason.is_none(), "{}", r.subject);
    }
}

#[test]
fn the_gate_reads_the_process_lock_row() {
    let m = matrix();
    assert_eq!(
        m.lookup(
            &id("lock.process"),
            "std-file-lock",
            "File::try_lock",
            &Config::new()
        ),
        Lookup::On
    );
    // The control: a backend nobody certified is not on.
    assert_ne!(
        m.lookup(
            &id("lock.process"),
            "marker-file",
            "File::try_lock",
            &Config::new()
        ),
        Lookup::On
    );
}

/// The key is the version of the library that RAN (`opendal::raw::VERSION`, the resolved one), not
/// the floor in `Cargo.toml` (`0.59.4` while the lock resolves `0.59.5`): the rows record the
/// behaviour of the library that was exercised. So a lock that resolves another opendal gets a
/// refusal naming it, until rows for it are recorded.
#[test]
fn opendal_rows_are_keyed_by_the_version_that_ran() {
    let m = matrix();
    let ran = OpendalBlob::memory().unwrap().version().to_string();
    let blob_rows: Vec<_> = m
        .rows()
        .iter()
        .filter(|r| r.backend == "opendal-fs" || r.backend == "opendal-memory")
        .collect();
    assert!(!blob_rows.is_empty());
    assert!(
        blob_rows.iter().any(|r| r.version == ran),
        "no opendal row for {ran}, the version that ran"
    );
    // Control: a version that did not run is not found.
    assert!(blob_rows.iter().all(|r| r.version != "0.0.1"));
}
