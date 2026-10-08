// SPDX-License-Identifier: MIT OR Apache-2.0
//! The blocking facade: for synchronous callers, with no runtime of their own.

use persistant::blocking::Store;
use persistant::{Config, Error, Need, Needs, Record};
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Seg {
    n: u32,
}
impl Record for Seg {
    const KIND: &'static str = "seg";
    const SCHEMA: u32 = 1;
}

fn needs() -> Needs {
    Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::Delete)
        .with(Need::AtomicReplace)
}

fn fs_config(root: &tempfile::TempDir, scratch: &tempfile::TempDir) -> Config {
    Config::Fs {
        root: root.path().to_path_buf(),
        atomic_write_dir: Some(scratch.path().to_path_buf()),
    }
}

#[test]
fn works_from_plain_synchronous_code() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let s = Store::open(fs_config(&root, &scratch), needs()).unwrap();
    s.replace("k", b"hello".to_vec()).unwrap();
    assert_eq!(s.read("k").unwrap(), b"hello");
    assert_eq!(std::fs::read(root.path().join("k")).unwrap(), b"hello");
    s.delete("k").unwrap();
    assert!(!root.path().join("k").exists());
}

#[test]
fn records_round_trip() {
    let s = Store::open(Config::Memory, needs()).unwrap();
    assert_eq!(s.get_record::<Seg>("a").unwrap(), None);
    s.put_record("a", &Seg { n: 7 }).unwrap();
    assert_eq!(s.get_record::<Seg>("a").unwrap(), Some(Seg { n: 7 }));
}

#[test]
fn refusal_surfaces_through_the_facade() {
    let root = tempfile::tempdir().unwrap();
    let cfg = Config::Fs {
        root: root.path().to_path_buf(),
        atomic_write_dir: None,
    };
    let err = Store::open(cfg, needs()).unwrap_err();
    assert!(matches!(err, Error::Refused { .. }), "{err:?}");
}

/// Calling a blocking facade from inside a tokio runtime must neither panic nor deadlock. Both
/// runtime flavours, since a current-thread runtime is the one that deadlocks if the facade
/// drives its work on the caller's thread.
#[tokio::test(flavor = "current_thread")]
async fn does_not_panic_or_deadlock_inside_a_current_thread_runtime() {
    let s = Store::open(Config::Memory, needs()).unwrap();
    s.replace("k", b"v".to_vec()).unwrap();
    assert_eq!(s.read("k").unwrap(), b"v");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn does_not_panic_inside_a_multi_thread_runtime() {
    let s = Store::open(Config::Memory, needs()).unwrap();
    s.replace("k", b"v".to_vec()).unwrap();
    assert_eq!(s.read("k").unwrap(), b"v");
}

#[test]
fn usable_from_many_threads_at_once() {
    let s = std::sync::Arc::new(Store::open(Config::Memory, needs()).unwrap());
    let handles: Vec<_> = (0..8u8)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || {
                for j in 0..20u8 {
                    let key = format!("k{i}");
                    s.replace(&key, vec![i, j]).unwrap();
                    assert_eq!(s.read(&key).unwrap()[0], i);
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn dropping_the_store_stops_its_worker() {
    // Opening and dropping many stores must not leak a thread each.
    for _ in 0..50 {
        let s = Store::open(Config::Memory, needs()).unwrap();
        s.replace("k", b"v".to_vec()).unwrap();
    }
}
