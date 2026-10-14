// SPDX-License-Identifier: MIT OR Apache-2.0
//! The checked-in `matrix/opendal.tsv` is readable by the gate's own reader, and says what the
//! findings in `conformance.rs` and `kv_on_fs.rs` say. This is the consumer's path: parse the
//! file, ask a question by `(subject, backend, version, config)`.

use persistant_blob::BlobBackend;
use persistant_core::{CapabilityId, Config, Lookup, Matrix, Reason};
use persistant_opendal::OpendalBlob;

fn matrix() -> Matrix {
    let text = include_str!("../../../matrix/opendal.tsv");
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
    // ... and with one, the reverse (README 0.4.7).
    assert_eq!(
        m.lookup(&id("blob.create_exclusive"), "opendal-fs", &version, &set),
        Lookup::Off(Reason::Missing)
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
