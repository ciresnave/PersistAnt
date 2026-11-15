// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify `KvOnRelational` and the relational kind over a real SQLite file (the plain and the durable
//! profile) and print the matrix file on stdout.
//!
//! `cargo run -q -p persistant-sqlx --features sqlite --example matrix_sqlite > matrix/sqlx-sqlite.tsv`
//!
//! CI regenerates this and fails if it differs from the checked-in `matrix/sqlx-sqlite.tsv`. Rows
//! are keyed by the SQLite library's version, so a bump of the bundled library is a visible change.

use persistant_core::erased::ErasedTrial;
use persistant_core::{Config, Matrix};
use persistant_kv::conformance::Leaf;
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_sqlx::{Bounded, SqliteBackend};
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let path = std::env::temp_dir().join(format!("persistant-matrix-{}.db", std::process::id()));
    let db = SqliteBackend::open(&path).await.expect("open");
    let rel_db = db.clone();
    let kv = KvOnRelational::open(db).await.expect("open kv");
    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    let config = Config::new().with("dialect", "sqlite");
    let mut rows: Vec<_> = persistant_kv::conformance::certify(&kv, &config, &env)
        .await
        .into_iter()
        .map(|c| c.row)
        .collect();
    // `text.index`: the text index over this kv.
    rows.push(
        persistant_kv::text_conformance::certify(&kv, kv.backend(), kv.version(), &config, &env)
            .await,
    );
    // The relational kind's own leaves, on the same server.
    rows.extend(
        persistant_relational::conformance::certify(&rel_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // The migration ledger: this file is the test own, so the ledger is too.
    rows.extend(
        persistant_relational::migrate_conformance::certify(&rel_db, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // The durable profile (`open_durable`: WAL, synchronous=FULL), which the registry offers as
    // `sqlite-full`. Key-value leaves only: that is the layer a registry handle serves.
    let full_path =
        std::env::temp_dir().join(format!("persistant-matrix-full-{}.db", std::process::id()));
    let full_db = SqliteBackend::open_durable(&full_path)
        .await
        .expect("open durable");
    let full_rel = full_db.clone();
    let full = KvOnRelational::open(full_db)
        .await
        .expect("open kv durable");
    rows.extend(
        persistant_kv::conformance::certify(&full, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    rows.push(
        persistant_kv::text_conformance::certify(
            &full,
            full.backend(),
            full.version(),
            &config,
            &env,
        )
        .await,
    );
    // The relational kind's leaves and the migration ledger on the durable profile too, so a
    // registry `rel.*` need finds rows for `sqlite-full` (it was listed for key-value only).
    rows.extend(
        persistant_relational::conformance::certify(&full_rel, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    rows.extend(
        persistant_relational::migrate_conformance::certify(&full_rel, &config, &env)
            .await
            .into_iter()
            .map(|c| c.row),
    );
    // `kv.bounded_call` with a call bound configured (row config `call_bound=set`), for each
    // profile; the unbounded runs above recorded it as `call_bound=unset`. The tables exist, so
    // opening through the bound only finds them.
    for (name, bounded_path, durable) in
        [("sqlite", &path, false), ("sqlite-full", &full_path, true)]
    {
        let inner = if durable {
            SqliteBackend::open_durable(bounded_path).await
        } else {
            SqliteBackend::open(bounded_path).await
        }
        .unwrap_or_else(|e| panic!("open {name} for the bound: {e}"));
        let bounded = KvOnRelational::open(Bounded::new(inner, Duration::from_millis(2000)))
            .await
            .expect("open bounded");
        rows.extend(
            persistant_kv::conformance::certify_leaves(
                &bounded,
                &config,
                &env,
                &[Leaf::BoundedCall],
            )
            .await
            .into_iter()
            .map(|c| c.row),
        );
    }
    // `kv.delete_erased`: the database file, `-wal` and `-journal` read back for a canary after
    // the delete, for each profile. A plain `kv.delete` is the known-bad control.
    for durable in [false, true] {
        let t = erase_trial(durable).await;
        let (kv, dir) = erase_kv(durable, "row").await;
        rows.push(persistant_core::erased::erased_row(
            "kv.delete_erased",
            kv.backend(),
            kv.version(),
            config.clone(),
            &env,
            "delete_erased::kv.delete_erased",
            &t,
        ));
        drop(kv);
        let _ = std::fs::remove_dir_all(&dir);
    }
    // `storage.secure_delete`: the `open_secure` profile (rollback journal, `secure_delete` on for
    // every connection). A canary value overwritten, and another deleted, by plain statements must
    // be in none of the files; the same overwrite on the default profile is the known-bad control.
    {
        let t = secure_trial().await;
        let (kv, dir) = open_kv(false, true, "row").await;
        rows.push(persistant_core::erased::erased_row(
            "storage.secure_delete",
            kv.backend(),
            kv.version(),
            config.clone().with("secure_delete", "on"),
            &env,
            "secure_delete::storage.secure_delete",
            &t,
        ));
        drop(kv);
        let _ = std::fs::remove_dir_all(&dir);
    }
    print!("{}", Matrix::from_rows(rows).to_tsv());
    for base in [&path, &full_path] {
        for ext in ["", "-wal", "-shm"] {
            let mut p = base.as_os_str().to_owned();
            p.push(ext);
            let _ = std::fs::remove_file(p);
        }
    }
}

const CANARY: &[u8] = b"CANARY-0c8e5d11-not-a-secret";

/// A fresh database of its own under the temp directory, with some ballast keys.
async fn erase_kv(durable: bool, tag: &str) -> (KvOnRelational<SqliteBackend>, std::path::PathBuf) {
    open_kv(durable, false, tag).await
}

async fn open_kv(
    durable: bool,
    secure: bool,
    tag: &str,
) -> (KvOnRelational<SqliteBackend>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "persistant-matrix-erase-{}-{}{}-{tag}",
        std::process::id(),
        u8::from(durable),
        if secure { "s" } else { "" }
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create dir");
    let path = dir.join("kv.sqlite");
    let db = if durable {
        SqliteBackend::open_durable(&path).await
    } else if secure {
        SqliteBackend::open_secure(&path).await
    } else {
        SqliteBackend::open(&path).await
    }
    .expect("open");
    let kv = KvOnRelational::open(db).await.expect("open kv");
    for i in 0..20 {
        kv.put(&format!("ballast/{i}"), vec![b'.'; 100], None)
            .await
            .expect("put");
    }
    (kv, dir)
}

fn holds_canary(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).expect("read dir").any(|e| {
        std::fs::read(e.expect("entry").path())
            .expect("read file")
            .windows(CANARY.len())
            .any(|w| w == CANARY)
    })
}

/// A plain delete on one database (the control: its canary stays in the files), an erased delete
/// on another (it must not).
async fn erase_trial(durable: bool) -> ErasedTrial {
    let (plain, plain_dir) = erase_kv(durable, "plain").await;
    plain
        .put("user/1", CANARY.to_vec(), None)
        .await
        .expect("put");
    let mut t = ErasedTrial {
        positive: holds_canary(&plain_dir),
        ..Default::default()
    };
    plain.delete("user/1").await.expect("delete");
    t.control_kept = holds_canary(&plain_dir);
    drop(plain);
    let _ = std::fs::remove_dir_all(&plain_dir);

    let (kv, dir) = erase_kv(durable, "erased").await;
    kv.put("user/1", CANARY.to_vec(), None).await.expect("put");
    match kv.delete_erased("user/1").await {
        Ok(()) => {
            t.erased = kv.get("user/1").await.expect("get").is_none() && !holds_canary(&dir);
        }
        Err(persistant_kv::KvError::Unsupported(m)) => {
            t.unsupported = Some(format!("kv does not support: {m}"));
        }
        Err(e) => panic!("delete_erased: {e}"),
    }
    drop(kv);
    let _ = std::fs::remove_dir_all(&dir);
    t
}

const CANARY_B: &[u8] = b"CANARY-8e19c3d4-not-a-secret";

/// The same plain statements on the default profile (the known-bad control: some canary stays in
/// the file) and on the secure profile (none may): two canary values written, one overwritten by a
/// short value and the other deleted.
async fn secure_trial() -> ErasedTrial {
    async fn run(secure: bool, tag: &str) -> (bool, bool) {
        let (kv, dir) = open_kv(false, secure, tag).await;
        kv.put("user/over", CANARY.to_vec(), None)
            .await
            .expect("put");
        kv.put("user/del", CANARY_B.to_vec(), None)
            .await
            .expect("put");
        let before = holds_bytes(&dir, CANARY) && holds_bytes(&dir, CANARY_B);
        kv.put("user/over", b"short".to_vec(), None)
            .await
            .expect("put");
        kv.delete("user/del").await.expect("delete");
        let after = holds_bytes(&dir, CANARY) || holds_bytes(&dir, CANARY_B);
        drop(kv);
        let _ = std::fs::remove_dir_all(&dir);
        (before, after)
    }
    let (positive, control_kept) = run(false, "ctl").await;
    let (_, secure_after) = run(true, "sec").await;
    ErasedTrial {
        positive,
        control_kept,
        erased: !secure_after,
        ..Default::default()
    }
}

fn holds_bytes(dir: &std::path::Path, needle: &[u8]) -> bool {
    std::fs::read_dir(dir).expect("read dir").any(|e| {
        std::fs::read(e.expect("entry").path())
            .expect("read file")
            .windows(needle.len())
            .any(|w| w == needle)
    })
}
