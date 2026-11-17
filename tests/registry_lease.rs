// SPDX-License-Identifier: MIT OR Apache-2.0
//! `lock.lease` and `lock.fencing` through the registry: listed for SQLite, refused for `memory` and
//! `fs`, and the opened handle's key-value backend takes a `Lease` whose token increases.
#![cfg(feature = "registry-sqlite")]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::kv::Lease;
use persistant::registry::{self, Endpoint};
use std::time::Duration;

fn needs() -> ProviderNeeds {
    let ids = [
        "kv.put_if_absent",
        "kv.compare_and_swap",
        "lock.lease",
        "lock.fencing",
    ]
    .map(|s| CapabilityId::new(s).unwrap());
    ProviderNeeds::new(ids.to_vec(), Durability::Volatile)
}

#[test]
fn the_sqlite_ids_are_listed_and_memory_and_fs_are_not() {
    let listed: Vec<&str> = registry::providers(&needs())
        .unwrap()
        .iter()
        .map(|l| l.id)
        .collect();
    assert!(
        listed.contains(&"sqlite") && listed.contains(&"sqlite-full"),
        "{listed:?}"
    );
    assert!(
        !listed.contains(&"memory") && !listed.contains(&"fs"),
        "{listed:?}"
    );
}

#[tokio::test]
async fn an_opened_sqlite_handle_serves_leases_with_increasing_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let h = registry::open_named("sqlite", &Endpoint::path(dir.path().join("l.db")), &needs())
        .await
        .unwrap();
    let long = Duration::from_secs(30);
    let a = Lease::acquire(h.kv(), "job", "A", long)
        .await
        .unwrap()
        .unwrap();
    assert!(
        Lease::acquire(h.kv(), "job", "B", long)
            .await
            .unwrap()
            .is_none()
    );
    let ta = a.token();
    assert!(a.release().await.unwrap());
    let b = Lease::acquire(h.kv(), "job", "B", long)
        .await
        .unwrap()
        .unwrap();
    assert!(b.token() > ta);
}

#[tokio::test]
async fn memory_is_refused_by_name_for_a_lease() {
    let r = registry::open_named("memory", &Endpoint::None, &needs()).await;
    assert!(r.is_err());
}
