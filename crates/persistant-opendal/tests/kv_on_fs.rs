// SPDX-License-Identifier: MIT OR Apache-2.0
//! `KvOnBlob` over real OpenDAL services: what the key-value leaves find, by blob configuration.
//!
//! The assertions are findings. They follow from the blob findings (`tests/conformance.rs`):
//! `fs` has `replace_atomic` only under `atomic_write_dir`; `create_exclusive` it has both with and
//! without one (under one, by linking a finished scratch file). So a strict `KvOnBlob` over `fs`
//! gives `kv.put` and `kv.put_if_absent` together when a scratch directory is set, and only
//! `kv.put_if_absent` when it is not.

use persistant_core::{Config, Reason};
use persistant_kv::KvOnBlob;
use persistant_kv::conformance::{Certified, certify};
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

/// Whether the directory's filesystem treats names that differ only in case as the same name
/// (Windows, default macOS). Probed, not assumed from the OS.
fn case_insensitive(dir: &std::path::Path) -> bool {
    std::fs::write(dir.join("CaseProbe"), b"x").unwrap();
    let same = dir.join("caseprobe").exists();
    std::fs::remove_file(dir.join("CaseProbe")).unwrap();
    same
}

/// `blob.read` and `kv.get` check that keys differing only in case are distinct keys, so a
/// case-insensitive filesystem finds them wrong; every other leaf is unaffected.
fn expected(id: &str, case_insensitive: bool) -> Option<Reason> {
    if case_insensitive && (id == "blob.read" || id == "kv.get") {
        Some(Reason::Wrong)
    } else {
        None
    }
}

fn merged(blob: &Config, kv: &Config) -> Config {
    let text = [blob.to_string(), kv.to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    Config::parse(&text).unwrap()
}

/// The leaves a blob store cannot give at all (no compare-and-delete).
const NEEDS_CAD: [&str; 13] = [
    "kv.compare_and_swap",
    "kv.compare_and_delete",
    "kv.take",
    // One-step count from a snapshot: a blob store can only page a listing.
    "kv.count",
    // The index layer needs `batch`.
    "kv.index",
    "lock.lease",
    "kv.purge_expired",
    // No multi-key transaction and no consistent snapshot on a blob store.
    "kv.batch_atomic.rollback",
    "kv.batch_atomic.concurrent_read",
    "kv.batch_atomic.optimistic_sequence",
    "kv.batch_atomic.absent_race",
    "kv.ordered_scan_values",
    // No call timeout is configured on a blob store.
    "kv.bounded_call",
];

#[tokio::test]
async fn over_the_memory_service_every_kv_leaf_but_the_cas_family() {
    let kv = KvOnBlob::new(OpendalBlob::memory().unwrap());
    let rows = certify(
        &kv,
        &merged(kv.blob().config(), &kv.config()),
        &Config::new(),
    )
    .await;
    for c in &rows {
        let id = c.leaf.id();
        if NEEDS_CAD.contains(&id) {
            assert_eq!(
                state(&rows, id),
                Some(Reason::Missing),
                "{}",
                summary(&rows)
            );
        } else {
            assert_eq!(state(&rows, id), None, "{}", summary(&rows));
        }
    }
    assert_eq!(rows[0].row.backend, "kv-on-blob:opendal-memory");
}

#[tokio::test]
async fn over_fs_without_atomic_write_dir_strict_put_is_refused_and_only_write_once_remains() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvOnBlob::new(OpendalBlob::fs(dir.path(), None).unwrap());
    let rows = certify(
        &kv,
        &merged(kv.blob().config(), &kv.config()),
        &Config::new(),
    )
    .await;
    assert_eq!(
        state(&rows, "kv.put"),
        Some(Reason::Missing),
        "{}",
        summary(&rows)
    );
    // Every other leaf's test begins by putting a value, so none can be certified, except the
    // write-once one: `put_if_absent` uses exclusive create and never `put`.
    for c in &rows {
        let expect_on = c.leaf.id() == "kv.put_if_absent";
        assert_eq!(c.row.enabled, expect_on, "{}", summary(&rows));
    }
    assert_eq!(rows[0].row.config.get("put_mode"), Some("atomic"));
    assert_eq!(rows[0].row.config.get("atomic_write_dir"), Some("unset"));
}

#[tokio::test]
async fn over_fs_without_atomic_write_dir_in_place_put_gives_put_if_absent_and_scans() {
    let dir = tempfile::tempdir().unwrap();
    let ci = case_insensitive(dir.path());
    let kv = KvOnBlob::with_in_place_put(OpendalBlob::fs(dir.path(), None).unwrap());
    let rows = certify(
        &kv,
        &merged(kv.blob().config(), &kv.config()),
        &Config::new(),
    )
    .await;
    for id in [
        "kv.get",
        "kv.put",
        "kv.delete",
        "kv.ttl",
        "kv.put_if_absent",
        "kv.prefix_list",
        "kv.ordered_scan",
    ] {
        assert_eq!(
            state(&rows, id),
            expected(id, ci),
            "{id}: {}",
            summary(&rows)
        );
    }
    for id in NEEDS_CAD {
        assert_eq!(
            state(&rows, id),
            Some(Reason::Missing),
            "{}",
            summary(&rows)
        );
    }
    assert_eq!(rows[0].row.config.get("put_mode"), Some("in_place"));
}

#[tokio::test]
async fn over_fs_with_atomic_write_dir_put_is_atomic_and_put_if_absent_is_on() {
    let root = tempfile::tempdir().unwrap();
    let ci = case_insensitive(root.path());
    let scratch = tempfile::tempdir().unwrap();
    let kv = KvOnBlob::new(OpendalBlob::fs(root.path(), Some(scratch.path())).unwrap());
    let rows = certify(
        &kv,
        &merged(kv.blob().config(), &kv.config()),
        &Config::new(),
    )
    .await;
    for id in [
        "kv.get",
        "kv.put",
        "kv.delete",
        "kv.ttl",
        "kv.put_if_absent",
        "kv.prefix_list",
        "kv.ordered_scan",
    ] {
        assert_eq!(
            state(&rows, id),
            expected(id, ci),
            "{id}: {}",
            summary(&rows)
        );
    }
    assert_eq!(rows[0].row.config.get("put_mode"), Some("atomic"));
    assert_eq!(rows[0].row.config.get("atomic_write_dir"), Some("set"));
}
