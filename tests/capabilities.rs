// SPDX-License-Identifier: MIT OR Apache-2.0
//! Capability refusal and atomic replace, on the `fs` and `memory` backends.

use persistant::{Config, Error, Need, Needs, Store};

fn atomic() -> Needs {
    Needs::new()
        .with(Need::Read)
        .with(Need::Write)
        .with(Need::AtomicReplace)
}

fn fs_config(root: &tempfile::TempDir, scratch: Option<&tempfile::TempDir>) -> Config {
    Config::Fs {
        root: root.path().to_path_buf(),
        atomic_write_dir: scratch.map(|s| s.path().to_path_buf()),
    }
}

/// (a) `fs` without `atomic_write_dir` cannot give atomic replace, and says so.
#[tokio::test]
async fn fs_without_atomic_write_dir_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let err = Store::open(fs_config(&root, None), atomic())
        .await
        .unwrap_err();
    match err {
        Error::Refused { backend, missing } => {
            assert_eq!(backend, "fs");
            assert_eq!(missing.len(), 1, "{missing:?}");
            assert_eq!(missing[0].need, Need::AtomicReplace);
            assert!(
                missing[0].detail.contains("atomic_write_dir"),
                "{}",
                missing[0].detail
            );
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

/// (a, positive control) the same backend with `atomic_write_dir` is accepted.
#[tokio::test]
async fn fs_with_atomic_write_dir_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    Store::open(fs_config(&root, Some(&scratch)), atomic())
        .await
        .unwrap();
}

/// (a, control) not declaring AtomicReplace is not a refusal: refusal follows the declaration.
#[tokio::test]
async fn fs_without_atomic_write_dir_is_fine_if_atomic_replace_not_declared() {
    let root = tempfile::tempdir().unwrap();
    let needs = Needs::new().with(Need::Read).with(Need::Write);
    Store::open(fs_config(&root, None), needs).await.unwrap();
}

/// (b) an interrupted write leaves the old content.
#[tokio::test]
async fn interrupted_replace_leaves_old_content_on_fs() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let store = Store::open(fs_config(&root, Some(&scratch)), atomic())
        .await
        .unwrap();
    store.replace("k", b"old".to_vec()).await.unwrap();
    {
        let mut r = store.begin_replace("k").await.unwrap();
        r.write(b"new-but-only-half".to_vec()).await.unwrap();
        // dropped without commit: the process died mid-write
    }
    assert_eq!(store.read("k").await.unwrap(), b"old");
    store.replace("k", b"new".to_vec()).await.unwrap();
    assert_eq!(store.read("k").await.unwrap(), b"new");
}

/// (b, control) OpenDAL's own `fs` without atomic_write_dir touches the target on an interrupted
/// write, which is why (a) refuses that configuration when atomic replace is declared.
#[tokio::test]
async fn raw_fs_without_atomic_dir_damages_old_content() {
    let root = tempfile::tempdir().unwrap();
    let op = opendal::Operator::new(
        opendal::services::Fs::default().root(&root.path().to_string_lossy()),
    )
    .unwrap();
    op.write("k", b"old".to_vec()).await.unwrap();
    let mut w = op.writer("k").await.unwrap();
    w.write(b"new-but-only-half".to_vec()).await.unwrap();
    drop(w);
    assert_ne!(std::fs::read(root.path().join("k")).unwrap(), b"old");
}

/// (c) a backend missing a declared need is refused with a named error.
#[tokio::test]
async fn backend_missing_a_declared_need_is_refused_by_name() {
    // Development backend has List; the masked one stands in for a production backend without it.
    let weaker = Config::Masked {
        inner: Box::new(Config::Memory),
        without: vec![Need::List],
    };
    let needs = Needs::new().with(Need::Read).with(Need::List);
    let err = Store::open(weaker, needs).await.unwrap_err();
    match err {
        Error::Refused { backend, missing } => {
            assert_eq!(backend, "memory");
            assert_eq!(missing.len(), 1, "{missing:?}");
            assert_eq!(missing[0].need, Need::List);
        }
        other => panic!("expected Refused, got {other:?}"),
    }
}

/// (c, control) the same declaration on the unmasked backend is accepted: the mask caused the refusal.
#[tokio::test]
async fn unmasked_backend_gives_the_same_need() {
    let needs = Needs::new().with(Need::Read).with(Need::List);
    Store::open(Config::Memory, needs).await.unwrap();
}

/// Memory advertises no ceiling, so any size is accepted (the refusal branch is unit-tested).
#[tokio::test]
async fn value_size_need_follows_the_advertised_ceiling() {
    let needs = Needs::new().with(Need::MaxValueSize(u64::MAX));
    Store::open(Config::Memory, needs).await.unwrap();
}

/// (c, control) memory gives everything else this change supports.
#[tokio::test]
async fn memory_accepts_the_common_needs() {
    let needs = atomic()
        .with(Need::Delete)
        .with(Need::Stat)
        .with(Need::List)
        .with(Need::CreateIfAbsent);
    let store = Store::open(Config::Memory, needs).await.unwrap();
    store.replace("a", b"1".to_vec()).await.unwrap();
    assert_eq!(store.read("a").await.unwrap(), b"1");
}

/// Read is gated by its declaration too.
#[tokio::test]
async fn read_requires_the_declaration() {
    let store = Store::open(Config::Memory, Needs::new()).await.unwrap();
    let err = store.read("k").await.unwrap_err();
    assert!(matches!(err, Error::NotDeclared(Need::Read)), "{err:?}");
}

/// No silent downgrade: replace is not offered unless AtomicReplace was declared.
#[tokio::test]
async fn replace_requires_the_declaration() {
    let needs = Needs::new().with(Need::Read).with(Need::Write);
    let store = Store::open(Config::Memory, needs).await.unwrap();
    let err = store.replace("k", b"x".to_vec()).await.unwrap_err();
    assert!(
        matches!(err, Error::NotDeclared(Need::AtomicReplace)),
        "{err:?}"
    );
}

/// Refused names ALL unmet needs, not the first.
#[tokio::test]
async fn refused_names_every_missing_need() {
    let root = tempfile::tempdir().unwrap();
    let weaker = Config::Masked {
        inner: Box::new(fs_config(&root, None)),
        without: vec![Need::List, Need::Stat],
    };
    let needs = atomic()
        .with(Need::List)
        .with(Need::Stat)
        .with(Need::Delete);
    let err = Store::open(weaker, needs).await.unwrap_err();
    let Error::Refused { missing, .. } = err else {
        panic!("expected Refused");
    };
    let named: Vec<Need> = missing.iter().map(|m| m.need).collect();
    assert_eq!(named, vec![Need::AtomicReplace, Need::List, Need::Stat]);
}

/// The real fs service really writes through the scratch path: while a replace is in flight the
/// scratch directory holds the partial data and the target still has the old content.
#[tokio::test]
async fn real_fs_writes_through_the_scratch_directory() {
    let root = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let store = Store::open(fs_config(&root, Some(&scratch)), atomic())
        .await
        .unwrap();
    store.replace("k", b"old".to_vec()).await.unwrap();
    let mut r = store.begin_replace("k").await.unwrap();
    r.write(b"new-data".to_vec()).await.unwrap();
    let scratch_files = std::fs::read_dir(scratch.path()).unwrap().count();
    assert!(scratch_files >= 1, "no temp file in the scratch directory");
    assert_eq!(std::fs::read(root.path().join("k")).unwrap(), b"old");
    r.commit().await.unwrap();
    assert_eq!(std::fs::read(root.path().join("k")).unwrap(), b"new-data");
    assert_eq!(std::fs::read_dir(scratch.path()).unwrap().count(), 0);
}

/// (b, memory) the same guarantee on `memory`, which `unmet` accepts on a claim about OpenDAL
/// internals: a dropped replace must leave the old value, so a future OpenDAL that inserts eagerly
/// fails here instead of silently breaking the refusal logic.
#[tokio::test]
async fn interrupted_replace_leaves_old_content_on_memory() {
    let store = Store::open(Config::Memory, atomic()).await.unwrap();
    store.replace("k", b"old".to_vec()).await.unwrap();
    {
        let mut r = store.begin_replace("k").await.unwrap();
        r.write(b"new-but-only-half".to_vec()).await.unwrap();
        // dropped without commit
    }
    assert_eq!(store.read("k").await.unwrap(), b"old");
    store.replace("k", b"new".to_vec()).await.unwrap();
    assert_eq!(store.read("k").await.unwrap(), b"new");
}

/// A need declared twice is reported once when refused.
#[tokio::test]
async fn a_duplicated_declaration_is_reported_once() {
    let weaker = Config::Masked {
        inner: Box::new(Config::Memory),
        without: vec![Need::Read],
    };
    let needs = Needs::new().with(Need::Read).with(Need::Read);
    let Err(Error::Refused { missing, .. }) = Store::open(weaker, needs).await else {
        panic!("expected a refusal");
    };
    assert_eq!(missing.len(), 1, "{missing:?}");
}

/// An absent key is a typed `NotFound` carrying the key, not a backend string.
#[tokio::test]
async fn reading_an_absent_key_is_not_found() {
    let store = Store::open(Config::Memory, atomic()).await.unwrap();
    let err = store.read("absent").await.unwrap_err();
    assert!(
        matches!(&err, Error::NotFound(k) if k == "absent"),
        "{err:?}"
    );
    store.replace("present", b"v".to_vec()).await.unwrap();
    assert_eq!(store.read("present").await.unwrap(), b"v");
}
