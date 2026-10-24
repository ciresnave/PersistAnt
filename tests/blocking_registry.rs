// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant::blocking::registry`: the registry for synchronous callers. Opened by id like the
//! async one, called from plain threads, from many threads at once and from inside a tokio runtime,
//! and refusing what the backend cannot give rather than approximating it.
#![cfg(feature = "registry")]

use persistant::blocking::registry::{self, KvHandle};
use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::kv::KvError;
use persistant::registry::{Endpoint, OpenError};
use std::sync::Arc;

fn needs(ids: &[&str], d: Durability) -> ProviderNeeds {
    ProviderNeeds::new(
        ids.iter()
            .map(|s| CapabilityId::new(*s).expect("a literal id"))
            .collect(),
        d,
    )
}

fn basic() -> ProviderNeeds {
    needs(
        &["kv.get", "kv.put", "kv.delete", "kv.prefix_list"],
        Durability::Process,
    )
}

/// The blob-backed in-process store: it opens on every platform.
fn open_memory() -> KvHandle {
    registry::open_named(
        "memory",
        &Endpoint::None,
        &needs(
            &["kv.get", "kv.put", "kv.delete", "kv.prefix_list"],
            Durability::Volatile,
        ),
    )
    .expect("memory opens")
}

#[test]
fn the_list_is_the_async_one_and_a_plain_thread_can_use_a_handle() {
    let listed = registry::providers(&basic()).expect("something qualifies");
    assert!(listed.iter().any(|l| l.id == "fs"), "{listed:?}");

    let kv = open_memory();
    assert_eq!(kv.id(), "memory");
    assert_eq!(kv.get("a").unwrap(), None);
    kv.put("svc/a", b"1".to_vec(), None).unwrap();
    kv.put("svc/b", b"2".to_vec(), None).unwrap();
    assert_eq!(kv.get("svc/a").unwrap().as_deref(), Some(&b"1"[..]));
    let page = kv.prefix_list("svc/", None, 10).unwrap();
    assert_eq!(page.keys.len(), 2);
    kv.delete("svc/a").unwrap();
    assert_eq!(kv.get("svc/a").unwrap(), None);
}

#[test]
fn what_the_backend_cannot_give_is_refused_not_approximated() {
    let kv = open_memory();
    // `memory` is a blob store: neither compare-and-delete nor a one-step count (the matrix says no).
    kv.put("k", b"v".to_vec(), None).unwrap();
    assert!(matches!(kv.take("k"), Err(KvError::Unsupported(_))));
    assert!(matches!(kv.count("k"), Err(KvError::Unsupported(_))));
    // And the value is still there: a refused take took nothing.
    assert_eq!(kv.get("k").unwrap().as_deref(), Some(&b"v"[..]));
}

#[test]
fn a_need_the_backend_does_not_meet_is_refused_at_open() {
    let dir = tempfile::tempdir().unwrap();
    // The registry's `fs` has no compare-and-swap: a blob store has no compare-and-delete.
    let wants = needs(
        &["kv.get", "kv.put", "kv.compare_and_swap"],
        Durability::Process,
    );
    let r = registry::open_named("fs", &Endpoint::path(dir.path().join("s")), &wants);
    assert!(matches!(r, Err(OpenError::Unmet(_))), "{r:?}");
    // The control: the same call with needs it does meet is not refused for them.
    let r = registry::open_named("fs", &Endpoint::path(dir.path().join("s")), &basic());
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn many_threads_share_one_handle_and_exactly_one_wins_a_race() {
    let kv = Arc::new(
        registry::open_named(
            "memory",
            &Endpoint::None,
            &needs(
                &["kv.get", "kv.put", "kv.put_if_absent"],
                Durability::Volatile,
            ),
        )
        .expect("memory opens"),
    );
    for round in 0..10 {
        let key = format!("race/{round}");
        let wins: usize = (0..8)
            .map(|i| {
                let (kv, key) = (kv.clone(), key.clone());
                std::thread::spawn(move || {
                    kv.put_if_absent(&key, vec![i as u8], None).unwrap() as usize
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum();
        assert_eq!(wins, 1, "round {round}: put_if_absent");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_from_inside_a_tokio_runtime_does_not_deadlock() {
    // The calling thread never drives the handle's runtime, so this is safe (it blocks this task's
    // thread for the duration, as any blocking call does).
    let kv = open_memory();
    kv.put("in/rt", b"x".to_vec(), None).unwrap();
    assert_eq!(kv.get("in/rt").unwrap().as_deref(), Some(&b"x"[..]));
}

#[cfg(feature = "registry-sqlite")]
#[test]
fn sqlite_gives_take_and_count_through_the_blocking_handle() {
    let dir = tempfile::tempdir().unwrap();
    let wants = needs(
        &["kv.get", "kv.put", "kv.take", "kv.count"],
        Durability::Process,
    );
    let kv = registry::open_named("sqlite", &Endpoint::path(dir.path().join("kv.db")), &wants)
        .expect("sqlite opens");
    for i in 0..3 {
        kv.put(&format!("c/{i}"), vec![i], None).unwrap();
    }
    assert_eq!(kv.count("c/").unwrap(), 3);
    assert_eq!(kv.take("c/1").unwrap(), Some(vec![1]));
    assert_eq!(kv.take("c/1").unwrap(), None);
    assert_eq!(kv.count("c/").unwrap(), 2);
}

#[cfg(feature = "registry-sqlite")]
#[test]
fn eight_threads_taking_one_key_on_sqlite_get_it_once() {
    let dir = tempfile::tempdir().unwrap();
    let wants = needs(&["kv.get", "kv.put", "kv.take"], Durability::Process);
    let kv = Arc::new(
        registry::open_named("sqlite", &Endpoint::path(dir.path().join("kv.db")), &wants)
            .expect("sqlite opens"),
    );
    for round in 0..10 {
        let key = format!("take/{round}");
        kv.put(&key, b"v".to_vec(), None).unwrap();
        let got: usize = (0..8)
            .map(|_| {
                let (kv, key) = (kv.clone(), key.clone());
                std::thread::spawn(move || usize::from(kv.take(&key).unwrap().is_some()))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|t| t.join().unwrap())
            .sum();
        assert_eq!(got, 1, "round {round}");
    }
}
