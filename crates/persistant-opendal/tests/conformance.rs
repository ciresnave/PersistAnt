// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runs the blob contract tests against real OpenDAL services and checks what they find.
//!
//! The assertions are findings about the service, not wishes: if OpenDAL changes, the test says
//! which leaf moved. The matrix a release records is the output of the same `certify` call.

use persistant_blob::conformance::{Certified, certify};
use persistant_core::{Config, Reason};
use persistant_opendal::OpendalBlob;

fn summary(rows: &[Certified]) -> String {
    rows.iter()
        .map(|c| {
            format!(
                "{}={}{}",
                c.leaf.id(),
                if c.row.enabled { "on" } else { "off" },
                c.row.reason.map(|r| format!("({r})")).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `None` = on; `Some(reason)` = off for that reason.
fn state(rows: &[Certified], id: &str) -> Option<Reason> {
    let c = rows
        .iter()
        .find(|c| c.leaf.id() == id)
        .unwrap_or_else(|| panic!("no row for {id}: {}", summary(rows)));
    if c.row.enabled { None } else { c.row.reason }
}

const SEQUENTIAL: [&str; 5] = [
    "blob.read",
    "blob.write",
    "blob.delete",
    "blob.stat",
    "blob.list",
];

#[tokio::test]
async fn memory_service_gives_every_certified_leaf() {
    let b = OpendalBlob::memory().unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for c in &rows {
        assert!(c.row.enabled, "{}", summary(&rows));
    }
    assert_eq!(rows[0].row.backend, "opendal-memory");
}

#[tokio::test]
async fn fs_without_atomic_write_dir_creates_exclusively_but_replaces_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(dir.path(), None).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for id in SEQUENTIAL {
        assert_eq!(state(&rows, id), None, "{}", summary(&rows));
    }
    assert_eq!(
        state(&rows, "blob.create_exclusive"),
        None,
        "{}",
        summary(&rows)
    );
    assert_eq!(
        state(&rows, "blob.replace_atomic.concurrent_read"),
        Some(Reason::Missing),
        "{}",
        summary(&rows)
    );
    assert_eq!(rows[0].row.config.get("atomic_write_dir"), Some("unset"));
    let ex = rows
        .iter()
        .find(|c| c.leaf.id() == "blob.create_exclusive")
        .unwrap();
    assert_eq!(
        ex.row.metadata.get("contenders"),
        Some("16"),
        "trial parameters are recorded"
    );
}

#[tokio::test]
async fn fs_with_atomic_write_dir_replaces_atomically_but_cannot_create_exclusively() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    for id in SEQUENTIAL {
        assert_eq!(state(&rows, id), None, "{}", summary(&rows));
    }
    assert_eq!(
        state(&rows, "blob.replace_atomic.concurrent_read"),
        None,
        "{}",
        summary(&rows)
    );
    assert_eq!(
        state(&rows, "blob.create_exclusive"),
        Some(Reason::Missing),
        "README 0.4.7: check-then-rename; {}",
        summary(&rows)
    );
    assert!(
        rows.iter()
            .all(|c| c.row.config.get("atomic_write_dir") == Some("set"))
    );
}

#[tokio::test]
async fn a_service_that_cannot_work_is_missing_not_wrong() {
    // Control: a root that is a file cannot hold keys; every leaf must be off, none "on".
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"x").unwrap();
    let b = OpendalBlob::fs(&file, None).unwrap();
    let rows = certify(&b, b.config(), &Config::new()).await;
    assert!(rows.iter().all(|c| !c.row.enabled), "{}", summary(&rows));
    assert!(
        rows.iter().any(|c| c.row.reason == Some(Reason::Missing)),
        "{}",
        summary(&rows)
    );
}
