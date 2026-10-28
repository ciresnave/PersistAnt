// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry: needs in, sorted backends out, the id kept in config opens the backend.
#![cfg(feature = "registry")]

use persistant::capability::CapabilityId;
use persistant::capability::choose::Durability;
use persistant::capability::provider::{DurabilityShortfall, ProviderNeeds};
use persistant::kv::KvBackend;
use persistant::registry::{self, Endpoint, OpenError, OpenOptions};
use std::time::Duration;

fn id(s: &str) -> CapabilityId {
    CapabilityId::new(s).unwrap()
}

fn basic(min: Durability) -> ProviderNeeds {
    ProviderNeeds::new(vec![id("kv.get"), id("kv.put"), id("kv.delete")], min)
}

#[test]
fn the_ids_a_program_stores_in_its_config_are_pinned() {
    // Append-only: this list only ever grows. A changed meaning is a new id.
    let want: &[&str] = &[
        "memory",
        "fs",
        #[cfg(feature = "registry-sqlite")]
        "sqlite",
        #[cfg(feature = "registry-sqlite")]
        "sqlite-full",
        #[cfg(feature = "registry-postgres")]
        "postgres",
        #[cfg(feature = "registry-mysql")]
        "mysql",
        #[cfg(feature = "registry-mysql")]
        "mariadb",
    ];
    assert_eq!(registry::known_ids(), want);
}

#[test]
fn a_need_lists_only_the_backends_that_have_all_of_it_and_says_why() {
    let list = registry::providers(&basic(Durability::Process)).unwrap();
    let ids: Vec<_> = list.iter().map(|l| l.id).collect();
    assert!(ids.contains(&"fs"), "{ids:?}");
    // Positive control for the exclusion: `memory` has the capabilities but is volatile, and the
    // same need at Volatile does list it.
    assert!(!ids.contains(&"memory"), "{ids:?}");
    let all = registry::providers(&basic(Durability::Volatile)).unwrap();
    assert!(all.iter().any(|l| l.id == "memory"));
    let fs = list.iter().find(|l| l.id == "fs").unwrap();
    let why = fs.why().join("\n");
    assert!(why.contains("durability: process"), "{why}");
    assert!(why.contains("not measured"), "{why}");
}

#[test]
fn nothing_satisfying_is_an_error_naming_each_backends_unmet_id() {
    let needs = ProviderNeeds::new(
        vec![id("kv.get"), id("kv.no_such_capability")],
        Durability::Volatile,
    );
    let e = registry::providers(&needs).unwrap_err();
    let named: Vec<_> = e.providers.iter().map(|p| p.provider.as_str()).collect();
    assert_eq!(named, registry::known_ids());
    for p in &e.providers {
        assert_eq!(p.missing.len(), 1, "{p:?}");
        assert_eq!(p.missing[0].0.as_str(), "kv.no_such_capability");
    }
    // Durability alone can rule everything out, and says so.
    let needs = ProviderNeeds::new(vec![id("kv.get")], Durability::Machine);
    if let Err(e) = registry::providers(&needs) {
        let mem = e.providers.iter().find(|p| p.provider == "memory").unwrap();
        assert_eq!(
            mem.durability,
            Some(DurabilityShortfall::Below(Durability::Volatile))
        );
    }
}

#[test]
fn preferred_capabilities_order_the_list_and_never_exclude() {
    let needs = basic(Durability::Volatile).prefer(vec![id("kv.ttl"), id("kv.no_such_capability")]);
    let list = registry::providers(&needs).unwrap();
    assert_eq!(
        list.len(),
        registry::known_ids().len(),
        "a preferred id never excludes"
    );
    for l in &list {
        assert_eq!(l.preferred.len(), 2);
        assert!(!l.preferred[1].1, "{}", l.id);
    }
    let why = list[0].why().join("\n");
    assert!(why.contains("lacks [kv.no_such_capability]"), "{why}");
}

#[tokio::test]
async fn open_named_gives_a_backend_agnostic_handle_for_memory_and_fs() {
    let needs = basic(Durability::Volatile);
    let h = registry::open_named("memory", &Endpoint::None, &needs)
        .await
        .unwrap();
    h.kv().put("k", b"v".to_vec(), None).await.unwrap();
    assert_eq!(h.kv().get("k").await.unwrap(), Some(b"v".to_vec()));
    assert_eq!(h.id(), "memory");

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("kv");
    let h = registry::open_named("fs", &Endpoint::path(&root), &basic(Durability::Process))
        .await
        .expect("fs opens on every filesystem");
    h.kv().put("a", b"1".to_vec(), None).await.unwrap();
    drop(h);
    // The same id and path opened again sees the data: this is what "stored in config" means.
    let h = registry::open_named("fs", &Endpoint::path(&root), &basic(Durability::Process))
        .await
        .unwrap();
    assert_eq!(h.kv().get("a").await.unwrap(), Some(b"1".to_vec()));
}

#[tokio::test]
async fn open_named_refuses_before_touching_anything() {
    let needs = basic(Durability::Volatile);
    let e = registry::open_named("nope", &Endpoint::None, &needs)
        .await
        .unwrap_err();
    match e {
        OpenError::UnknownBackend { id, known } => {
            assert_eq!(id, "nope");
            assert_eq!(known, registry::known_ids());
        }
        other => panic!("{other:?}"),
    }
    let dir = tempfile::tempdir().unwrap();
    let e = registry::open_named("fs", &Endpoint::None, &needs)
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            OpenError::WrongEndpoint {
                id: "fs",
                got: "none",
                ..
            }
        ),
        "{e:?}"
    );
    let e = registry::open_named("memory", &Endpoint::path(dir.path()), &needs)
        .await
        .unwrap_err();
    assert!(
        matches!(e, OpenError::WrongEndpoint { id: "memory", .. }),
        "{e:?}"
    );
    // Needs the backend does not meet: refused with the unmet id, and the directory is not created.
    let root = dir.path().join("never");
    let unmet = ProviderNeeds::new(vec![id("kv.no_such_capability")], Durability::Volatile);
    let e = registry::open_named("fs", &Endpoint::path(&root), &unmet)
        .await
        .unwrap_err();
    match e {
        OpenError::Unmet(u) => assert_eq!(
            u.providers[0].missing[0].0.as_str(),
            "kv.no_such_capability"
        ),
        other => panic!("{other:?}"),
    }
    assert!(!root.exists(), "the gate ran before any I/O");
    // Durability is part of the gate at open: memory is volatile.
    let e = registry::open_named("memory", &Endpoint::None, &basic(Durability::Process))
        .await
        .unwrap_err();
    assert!(matches!(e, OpenError::Unmet(_)), "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_open_future_and_the_handle_are_send() {
    let needs = basic(Durability::Volatile);
    let t = tokio::spawn(async move {
        let h = registry::open_named("memory", &Endpoint::None, &needs)
            .await
            .unwrap();
        h.kv().put("k", vec![1], None).await.unwrap();
        h.kv().get("k").await.unwrap()
    });
    assert_eq!(t.await.unwrap(), Some(vec![1]));
}

#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn sqlite_and_sqlite_full_are_told_apart_in_the_list_text() {
    let list = registry::providers(&basic(Durability::Process)).unwrap();
    let plain = list
        .iter()
        .find(|l| l.id == "sqlite")
        .unwrap()
        .why()
        .join("\n");
    let full = list
        .iter()
        .find(|l| l.id == "sqlite-full")
        .unwrap()
        .why()
        .join("\n");
    assert!(plain.contains("durability: process"), "{plain}");
    assert!(plain.contains("Pick `sqlite-full`"), "{plain}");
    assert!(
        full.contains("durability: machine (verified: machine)"),
        "{full}"
    );
    assert!(full.contains("synchronous=FULL"), "{full}");
    // Asking for Machine lists the durable profile and not the default one.
    let machine: Vec<_> = registry::providers(&basic(Durability::Machine))
        .unwrap()
        .iter()
        .map(|l| l.id)
        .collect();
    assert!(machine.contains(&"sqlite-full"), "{machine:?}");
    assert!(!machine.contains(&"sqlite"), "{machine:?}");

    let dir = tempfile::tempdir().unwrap();
    for name in ["sqlite", "sqlite-full"] {
        let file = dir.path().join(format!("{name}.db"));
        let h = registry::open_named(name, &Endpoint::path(&file), &basic(Durability::Process))
            .await
            .unwrap();
        h.kv().put("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(h.kv().get("k").await.unwrap(), Some(b"v".to_vec()));
        assert_eq!(h.kv().backend(), format!("kv-on-relational:{name}"));
    }
    // sqlite refuses a Machine need at open.
    let file = dir.path().join("refused.db");
    let e = registry::open_named(
        "sqlite",
        &Endpoint::path(&file),
        &basic(Durability::Machine),
    )
    .await
    .unwrap_err();
    assert!(matches!(e, OpenError::Unmet(_)), "{e:?}");
}

fn bounded() -> ProviderNeeds {
    ProviderNeeds::new(
        vec![id("kv.get"), id("kv.put"), id("kv.bounded_call")],
        Durability::Volatile,
    )
}

#[test]
fn a_bounded_need_is_unmet_by_memory_and_fs_whatever_else_is_built() {
    match registry::providers(&bounded()) {
        Err(e) => {
            // Nothing built can be bounded: every provider is named with the unmet id.
            assert_eq!(e.providers.len(), registry::known_ids().len());
            assert!(e.providers.iter().all(|p| {
                p.missing
                    .iter()
                    .any(|(i, _)| i.as_str() == "kv.bounded_call")
            }));
        }
        Ok(list) => {
            let ids: Vec<_> = list.iter().map(|l| l.id).collect();
            assert!(!ids.contains(&"memory") && !ids.contains(&"fs"), "{ids:?}");
            assert!(list[0].why().join("\n").contains("OpenOptions::call_bound"));
        }
    }
}

#[tokio::test]
async fn into_kv_gives_an_owned_store_for_a_generic_consumer() {
    async fn consumer<K: KvBackend>(kv: K) -> Option<Vec<u8>> {
        kv.put("k", b"v".to_vec(), None).await.unwrap();
        kv.get("k").await.unwrap()
    }
    let h = registry::open_named("memory", &Endpoint::None, &basic(Durability::Volatile))
        .await
        .unwrap();
    assert_eq!(consumer(h.into_kv()).await, Some(b"v".to_vec()));
}

#[tokio::test]
async fn a_call_bound_is_refused_where_it_cannot_apply_and_required_where_needed() {
    let opts = OpenOptions::new().call_bound(Duration::from_secs(2));
    let e = registry::open_named_with(
        "memory",
        &Endpoint::None,
        &basic(Durability::Volatile),
        &opts,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(e, OpenError::BoundUnsupported { id: "memory" }),
        "{e:?}"
    );
    // A bounded need without a bound: refused before any I/O (the directory is not created).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("never");
    let e = registry::open_named("fs", &Endpoint::path(&root), &bounded())
        .await
        .unwrap_err();
    assert!(matches!(e, OpenError::BoundRequired { id: "fs" }), "{e:?}");
    assert!(!root.exists());
}

#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn sqlite_lists_kv_bounded_call_and_opens_bounded_only_with_a_bound() {
    let list = registry::providers(&bounded()).unwrap();
    let ids: Vec<_> = list.iter().map(|l| l.id).collect();
    assert!(
        ids.contains(&"sqlite") && ids.contains(&"sqlite-full"),
        "{ids:?}"
    );
    assert!(!ids.contains(&"memory") && !ids.contains(&"fs"), "{ids:?}");
    let dir = tempfile::tempdir().unwrap();
    for name in ["sqlite", "sqlite-full"] {
        let file = dir.path().join(format!("{name}.db"));
        let e = registry::open_named(name, &Endpoint::path(&file), &bounded())
            .await
            .unwrap_err();
        assert!(matches!(e, OpenError::BoundRequired { .. }), "{e:?}");
        let opts = OpenOptions::new().call_bound(Duration::from_secs(5));
        let h = registry::open_named_with(name, &Endpoint::path(&file), &bounded(), &opts)
            .await
            .unwrap();
        assert_eq!(h.kv().call_bound(), Some(Duration::from_secs(5)));
        h.kv().put("k", b"v".to_vec(), None).await.unwrap();
        assert_eq!(h.kv().get("k").await.unwrap(), Some(b"v".to_vec()));
    }
    // Opened without the need and without a bound, the same backend reports no bound.
    let file = dir.path().join("plain.db");
    let h = registry::open_named(
        "sqlite",
        &Endpoint::path(&file),
        &basic(Durability::Process),
    )
    .await
    .unwrap();
    assert_eq!(h.kv().call_bound(), None);
}

// ---- the blob and relational kinds are listed (docs/DESIGN-REGISTRY-KINDS.md, work order 1) ----

fn blob_need(min: Durability) -> ProviderNeeds {
    ProviderNeeds::new(
        vec![id("blob.read"), id("blob.write"), id("blob.stat")],
        min,
    )
}

fn listed(needs: &ProviderNeeds) -> Vec<&'static str> {
    registry::providers(needs)
        .unwrap()
        .iter()
        .map(|l| l.id)
        .collect()
}

#[test]
fn a_blob_need_lists_the_blob_backends_and_no_sql_database() {
    let ids = listed(&blob_need(Durability::Volatile));
    assert!(ids.contains(&"memory") && ids.contains(&"fs"), "{ids:?}");
    // Control: the same build lists SQL backends for a kv need, so their absence here is the need's doing.
    let kv_ids = listed(&basic(Durability::Volatile));
    #[cfg(feature = "registry-sqlite")]
    {
        assert!(kv_ids.contains(&"sqlite"), "{kv_ids:?}");
        assert!(
            !ids.contains(&"sqlite") && !ids.contains(&"sqlite-full"),
            "{ids:?}"
        );
    }
    assert!(kv_ids.len() >= ids.len());
}

#[test]
fn the_durability_of_a_blob_need_is_the_blob_stores() {
    // opendal-memory declares Volatile, opendal-fs Process.
    let ids = listed(&blob_need(Durability::Process));
    assert_eq!(ids, ["fs"]);
    let e = registry::providers(&blob_need(Durability::Machine)).unwrap_err();
    assert!(
        e.providers
            .iter()
            .any(|p| p.provider == "fs" && p.durability.is_some()),
        "{e}"
    );
}

#[test]
fn a_need_naming_two_kinds_is_met_only_where_both_are_served() {
    let both = ProviderNeeds::new(
        vec![id("kv.get"), id("kv.put"), id("blob.stat")],
        Durability::Volatile,
    );
    let ids = listed(&both);
    assert!(ids.contains(&"memory") && ids.contains(&"fs"), "{ids:?}");
    #[cfg(feature = "registry-sqlite")]
    assert!(!ids.contains(&"sqlite"), "{ids:?}");
    let list = registry::providers(&both).unwrap();
    assert!(
        list.iter()
            .all(|l| l.why().iter().any(|w| w.contains("weakest"))),
        "a multi-kind need says whose durability is shown"
    );
}

#[test]
fn a_need_no_provider_serves_is_unmet_and_names_each_backends_missing_id() {
    // Blob and relational at once: no provider has both layers.
    let n = ProviderNeeds::new(
        vec![id("blob.stat"), id("rel.transaction")],
        Durability::Volatile,
    );
    let e = registry::providers(&n).unwrap_err();
    let fs = e.providers.iter().find(|p| p.provider == "fs").unwrap();
    assert!(
        fs.missing
            .iter()
            .any(|(i, _)| i.as_str() == "rel.transaction")
            && !fs.missing.iter().any(|(i, _)| i.as_str() == "blob.stat"),
        "{e}"
    );
}

#[cfg(feature = "registry-sqlite")]
#[test]
fn a_relational_need_lists_sqlite_and_not_the_blob_stores() {
    let n = ProviderNeeds::new(
        vec![id("rel.values"), id("rel.transaction")],
        Durability::Volatile,
    );
    let ids = listed(&n);
    assert!(ids.contains(&"sqlite"), "{ids:?}");
    assert!(!ids.contains(&"memory") && !ids.contains(&"fs"), "{ids:?}");
    // `sqlite-full` has no relational rows yet (docs/DESIGN-REGISTRY-KINDS.md 5.1): no row is off, so it is
    // not listed. This pins the gap; certifying the relational leaves for it ends the gap and this line.
    assert!(!ids.contains(&"sqlite-full"), "{ids:?}");
}

#[cfg(feature = "registry-sqlite")]
fn rel_need() -> ProviderNeeds {
    ProviderNeeds::new(
        vec![id("rel.values"), id("rel.transaction")],
        Durability::Volatile,
    )
}

#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn the_rel_handle_passes_the_same_contract_as_the_driver() {
    use persistant::capability::Config;
    use persistant::relational::RelationalBackend;
    use persistant::relational::conformance::certify;
    use persistant::sqlx_driver::SqliteBackend;

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.sqlite");
    let h = registry::open_named("sqlite", &Endpoint::path(&file), &rel_need())
        .await
        .unwrap();
    let rel = h.rel().expect("a rel need gets a relational handle");
    let direct = SqliteBackend::open(&dir.path().join("direct.sqlite"))
        .await
        .unwrap();
    let through = certify(
        rel,
        &Config::new().with("dialect", "sqlite"),
        &Config::new(),
    )
    .await;
    let plain = certify(
        &direct,
        &Config::new().with("dialect", "sqlite"),
        &Config::new(),
    )
    .await;
    let sum = |rows: &[persistant::relational::conformance::Certified]| -> Vec<(String, bool)> {
        rows.iter()
            .map(|c| (c.leaf.id().to_string(), c.row.enabled))
            .collect()
    };
    assert_eq!(sum(&through), sum(&plain));
    // Control: the contract ran, and something passed.
    assert!(through.iter().any(|c| c.row.enabled));
    assert_eq!(rel.backend(), direct.backend());
}

#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn the_rel_handle_runs_statements_and_transactions_and_exists_only_when_asked_for() {
    use persistant::relational::{Isolation, RelationalBackend, RelationalTx, Value};

    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.sqlite");
    // A kv-only need: no relational handle (the kv store is there regardless).
    let h = registry::open_named(
        "sqlite",
        &Endpoint::path(&file),
        &basic(Durability::Volatile),
    )
    .await
    .unwrap();
    assert!(h.rel().is_none());
    drop(h);

    let h = registry::open_named("sqlite", &Endpoint::path(&file), &rel_need())
        .await
        .unwrap();
    let rel = h.rel().unwrap();
    assert_eq!(rel.dialect(), persistant::relational::Dialect::Sqlite);
    assert_eq!(rel.call_bound(), None);
    rel.execute("CREATE TABLE t (a INTEGER)", &[])
        .await
        .unwrap();
    // A rolled-back transaction leaves nothing; a committed one does.
    let mut tx = rel.begin(Isolation::default()).await.unwrap();
    tx.execute("INSERT INTO t (a) VALUES (?)", &[Value::Int(1)])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(rel.query("SELECT a FROM t", &[]).await.unwrap().is_empty());
    let mut tx = rel.begin(Isolation::default()).await.unwrap();
    tx.execute("INSERT INTO t (a) VALUES (?)", &[Value::Int(2)])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(rel.query("SELECT a FROM t", &[]).await.unwrap().len(), 1);
    // The key-value store shares the file and is untouched by the table.
    h.kv().put("k", b"v".to_vec(), None).await.unwrap();
    assert_eq!(h.kv().get("k").await.unwrap().unwrap(), b"v");

    // A call bound is carried through to the relational handle.
    let opts = OpenOptions::new().call_bound(Duration::from_secs(7));
    let b = registry::open_named_with("sqlite", &Endpoint::path(&file), &rel_need(), &opts)
        .await
        .unwrap();
    assert_eq!(b.rel().unwrap().call_bound(), Some(Duration::from_secs(7)));
    let mut tx = b.rel().unwrap().begin(Isolation::default()).await.unwrap();
    assert_eq!(tx.query("SELECT a FROM t", &[]).await.unwrap().len(), 1);
    tx.rollback().await.unwrap();
}

#[cfg(feature = "registry-sqlite")]
#[tokio::test]
async fn blob_and_rel_together_open_both_and_a_rel_need_on_a_blob_store_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blobs");
    let e = registry::open_named("fs", &Endpoint::path(&root), &rel_need())
        .await
        .unwrap_err();
    assert!(matches!(e, OpenError::Unmet(_)), "{e:?}");
    assert!(!root.exists());
    let file = dir.path().join("y.sqlite");
    let both = ProviderNeeds::new(
        vec![id("rel.values"), id("blob.read")],
        Durability::Volatile,
    );
    // No provider serves blob and rel both (sqlite has no blob row): unmet, nothing opened.
    let e = registry::open_named("sqlite", &Endpoint::path(&file), &both)
        .await
        .unwrap_err();
    assert!(matches!(e, OpenError::Unmet(_)), "{e:?}");
    assert!(!file.exists());
}

fn summary(rows: &[persistant::blob::conformance::Certified]) -> Vec<(String, bool)> {
    rows.iter()
        .map(|c| (c.leaf.id().to_string(), c.row.enabled))
        .collect()
}

#[tokio::test]
async fn the_blob_handle_passes_the_same_contract_as_the_driver_on_memory_and_fs() {
    use persistant::blob::BlobBackend;
    use persistant::blob::conformance::certify;
    use persistant::capability::Config;
    use persistant::opendal_driver::OpendalBlob;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("blobs");
    let direct_dir = dir.path().join("direct");
    std::fs::create_dir_all(&direct_dir).unwrap();
    let scratch = dir.path().join("direct.persistant-atomic");
    std::fs::create_dir_all(&scratch).unwrap();
    let cases: [(&str, Endpoint, OpendalBlob); 2] = [
        ("memory", Endpoint::None, OpendalBlob::memory().unwrap()),
        (
            "fs",
            Endpoint::path(&root),
            OpendalBlob::fs(&direct_dir, Some(&scratch)).unwrap(),
        ),
    ];
    for (name, endpoint, direct) in cases {
        let h = registry::open_named(name, &endpoint, &blob_need(Durability::Volatile))
            .await
            .unwrap();
        let blob = h.blob().expect("a blob need gets a blob handle");
        let through = certify(blob, blob.config(), &Config::new()).await;
        let plain = certify(&direct, direct.config(), &Config::new()).await;
        assert_eq!(summary(&through), summary(&plain), "{name}");
        // Control: the contract ran (a result per leaf, and at least one on).
        assert!(through.iter().any(|c| c.row.enabled), "{name}");
        assert_eq!(blob.backend(), direct.backend());
    }
}

#[tokio::test]
async fn the_blob_handle_refuses_the_keys_the_key_value_store_keeps() {
    use persistant::blob::BlobBackend;

    let dir = tempfile::tempdir().unwrap();
    for (name, endpoint) in [
        ("memory", Endpoint::None),
        ("fs", Endpoint::path(dir.path().join("b"))),
    ] {
        let both = ProviderNeeds::new(
            vec![
                id("kv.get"),
                id("kv.put"),
                id("blob.read"),
                id("blob.write"),
            ],
            Durability::Volatile,
        );
        let h = registry::open_named(name, &endpoint, &both).await.unwrap();
        let blob = h.blob().unwrap();
        for key in ["kv", "kvx", "kv/a", "kvx/6b/x"] {
            let e = blob.write(key, b"x".to_vec()).await.unwrap_err();
            assert!(
                e.to_string().contains("key-value store"),
                "{name} {key}: {e}"
            );
            assert!(blob.read(key).await.is_err(), "{name} {key}");
            assert!(blob.delete(key).await.is_err(), "{name} {key}");
            assert!(blob.stat(key).await.is_err(), "{name} {key}");
        }
        assert!(blob.list("kv/", None, 10).await.is_err());
        assert!(blob.list("kvx/", None, 10).await.is_err());
        // Control: names that only start like a reserved one are ordinary keys.
        for key in ["kvs/a", "kvx2", "kvx.txt", "k"] {
            blob.write(key, b"ok".to_vec()).await.unwrap();
            assert_eq!(
                blob.read(key).await.unwrap().unwrap(),
                b"ok",
                "{name} {key}"
            );
        }
        // The key-value store writes through the same directory, and a listing never shows it.
        h.kv().put("some key", b"v".to_vec(), None).await.unwrap();
        let page = blob.list("", None, 100).await.unwrap();
        let keys: Vec<&str> = page.entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["k", "kvs/a", "kvx.txt", "kvx2"], "{name}");
        assert_eq!(h.kv().get("some key").await.unwrap().unwrap(), b"v");
    }
}

#[tokio::test]
async fn the_blob_handle_exists_only_for_a_need_that_named_a_blob_id() {
    let h = registry::open_named("memory", &Endpoint::None, &basic(Durability::Volatile))
        .await
        .unwrap();
    assert!(h.blob().is_none());
    let h = registry::open_named("memory", &Endpoint::None, &blob_need(Durability::Volatile))
        .await
        .unwrap();
    assert!(h.blob().is_some());
    // The key-value store is always there (additive).
    h.kv().put("k", b"v".to_vec(), None).await.unwrap();
}
