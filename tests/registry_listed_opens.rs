// SPDX-License-Identifier: MIT OR Apache-2.0
//! `providers` and `open_named` must agree: every id `providers(&needs)` lists for a need can pass
//! `open_named`'s gate for that need. Over every id this build offers and every capability in the
//! vocabulary, alone and beside `kv.get`. A listed id that opens `Unmet` is a defect (0.18.12:
//! `sqlite-full` was listed for `rel.values` and refused `NoRow` at open).
//!
//! The server ids are not opened here (a port nothing listens on costs a connection timeout per
//! listing: 48 minutes for the full vocabulary); `registry_servers.rs` runs the same check against
//! the real servers the CI jobs provide.
#![cfg(feature = "registry")]

use persistant::capability::choose::Durability;
use persistant::capability::provider::ProviderNeeds;
use persistant::capability::vocabulary;
use persistant::registry::{self, Endpoint, OpenError};

fn endpoint_for(id: &str, dir: &std::path::Path) -> Endpoint {
    match id {
        "memory" => Endpoint::None,
        "fs" => Endpoint::path(dir.join("fs")),
        "sqlite" | "sqlite-full" | "sqlite-secure" => Endpoint::path(dir.join(format!("{id}.db"))),
        other => panic!("an id this test does not know how to open: {other}"),
    }
}

#[tokio::test]
async fn every_listed_provider_passes_the_open_gate_for_the_same_needs() {
    let dir = tempfile::tempdir().unwrap();
    let mut checked = 0;
    let mut refused: Vec<String> = Vec::new();
    let mut full_rel = 0;
    for (cap, _) in vocabulary::seed() {
        // `kv.bounded_call` needs a call bound at open; the options carry none here.
        if cap.as_str() == "kv.bounded_call" {
            continue;
        }
        let beside_kv = cap.as_str().starts_with("kv.") || cap.as_str().starts_with("lock.");
        let variants: Vec<Vec<_>> = if beside_kv {
            vec![vec![cap.clone()]]
        } else {
            vec![
                vec![cap.clone()],
                vec![
                    persistant::capability::CapabilityId::new("kv.get").unwrap(),
                    cap.clone(),
                ],
            ]
        };
        for ids in variants {
            let needs = ProviderNeeds::new(ids.clone(), Durability::Volatile);
            let Ok(listed) = registry::providers(&needs) else {
                continue;
            };
            for l in listed {
                if matches!(l.id, "postgres" | "mysql" | "mariadb") {
                    continue;
                }
                checked += 1;
                #[cfg(feature = "registry-sqlite")]
                if l.id == "sqlite-full" && cap.as_str().starts_with("rel.") {
                    full_rel += 1;
                }
                let d = dir.path().join(format!("{checked}"));
                std::fs::create_dir_all(&d).unwrap();
                match registry::open_named(l.id, &endpoint_for(l.id, &d), &needs).await {
                    Err(OpenError::Unmet(u)) => refused.push(format!(
                        "{} listed for {:?} but open_named says Unmet: {u:?}",
                        l.id,
                        ids.iter().map(|i| i.as_str()).collect::<Vec<_>>()
                    )),
                    _ => {}
                }
            }
        }
    }
    assert!(
        checked > 20,
        "control: the test looked at {checked} listings only"
    );
    assert!(
        refused.is_empty(),
        "{} listing(s) cannot be opened:\n{}",
        refused.len(),
        refused.join("\n")
    );
}

/// The case that failed in 0.18.12, end to end: `sqlite-full` opens for relational needs and the
/// relational handle runs a statement.
#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn sqlite_full_opens_for_relational_needs_and_runs_a_statement() {
    use persistant::capability::CapabilityId;
    use persistant::relational::{RelationalBackend, Value};
    let dir = tempfile::tempdir().unwrap();
    let needs = ProviderNeeds::new(
        vec![
            CapabilityId::new("rel.values").unwrap(),
            CapabilityId::new("rel.transaction").unwrap(),
        ],
        Durability::Machine,
    );
    let h = registry::open_named(
        "sqlite-full",
        &Endpoint::path(dir.path().join("k.db")),
        &needs,
    )
    .await
    .unwrap();
    let rel = h.rel().expect("a rel need gets a relational handle");
    assert_eq!(rel.backend(), "sqlite-full");
    rel.execute("CREATE TABLE t (n INTEGER)", &[])
        .await
        .unwrap();
    rel.execute("INSERT INTO t VALUES ($1)", &[Value::Int(7)])
        .await
        .unwrap();
    let rows = rel.query("SELECT n FROM t", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
}
