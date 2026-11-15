// SPDX-License-Identifier: MIT OR Apache-2.0
//! Certify the OpenDAL blob driver and `KvOnBlob` over it, and print the matrix file on stdout.
//!
//! `cargo run -q -p persistant-opendal --example opendal_matrix > matrix/opendal.tsv`
//!
//! CI regenerates this on Linux and fails if it differs from the checked-in `matrix/opendal.tsv`,
//! so the file is a record of what the code certified, not a claim. No clock field: the commit
//! date is the date. Environment (`os`, `arch`) is in each row's metadata, because case
//! sensitivity of the filesystem is a fact about the machine, not the crate.

use persistant_blob::BlobBackend;
use persistant_core::{Config, Matrix, Row};
use persistant_kv::{KvBackend, KvOnBlob};
use persistant_opendal::ErasedTrial;
use persistant_opendal::OpendalBlob;
use std::path::Path;

fn clean(rows: impl IntoIterator<Item = Row>, tmp: &[&str]) -> Vec<Row> {
    rows.into_iter()
        .map(|mut r| {
            for t in tmp {
                r.note = r.note.replace(t, "<tmp>");
            }
            r
        })
        .collect()
}

fn merged(a: &Config, b: &Config) -> Config {
    let text = [a.to_string(), b.to_string()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(";");
    Config::parse(&text).expect("config facts parse")
}

fn main() {
    // This binary is its own crash-test child (`blob.replace_atomic`): it returns at once unless
    // it was started that way.
    persistant_opendal::crash::maybe_run_child();
    // And the child of the `lock.process` test.
    persistant_opendal::maybe_run_lock_child();

    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    // The crash half of `blob.replace_atomic` kills child processes, so it runs before any
    // runtime exists.
    let work = tempfile::tempdir().unwrap();
    let crash_rows =
        persistant_opendal::crash::certify_replace(work.path(), &[], &env).expect("crash test ran");
    let tmp_work = work.path().to_string_lossy().into_owned();
    let crash_rows = clean(crash_rows, &[tmp_work.as_str()]);
    let lock_work = tempfile::tempdir().unwrap();
    let lock_row =
        persistant_opendal::certify_process_lock(lock_work.path(), &env).expect("lock test ran");

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(run(env, crash_rows, lock_row));
}

async fn run(env: Config, crash_rows: Vec<Row>, lock_row: Row) {
    let mut rows: Vec<Row> = Vec::new();

    let root_a = tempfile::tempdir().unwrap();
    let root_b = tempfile::tempdir().unwrap();
    let root_c = tempfile::tempdir().unwrap();
    let root_d = tempfile::tempdir().unwrap();
    let scratch_b = tempfile::tempdir().unwrap();
    let scratch_d = tempfile::tempdir().unwrap();
    let tmp: Vec<String> = [&root_a, &root_b, &root_c, &root_d, &scratch_b, &scratch_d]
        .iter()
        .map(|d| d.path().to_string_lossy().into_owned())
        .collect();
    let tmp: Vec<&str> = tmp.iter().map(String::as_str).collect();

    // Blob driver.
    let mem = OpendalBlob::memory().unwrap();
    let fs_unset = OpendalBlob::fs(root_a.path(), None).unwrap();
    let fs_set = OpendalBlob::fs(root_b.path(), Some(scratch_b.path())).unwrap();
    for b in [&mem, &fs_unset, &fs_set] {
        let certified = persistant_blob::conformance::certify(b, b.config(), &env).await;
        rows.extend(clean(certified.into_iter().map(|c| c.row), &tmp));
    }

    rows.extend(crash_rows);
    rows.push(lock_row.clone());

    // `blob.file_mode`: every file an owner-only `fs` driver creates, read back.
    let mode_work = tempfile::tempdir().unwrap();
    let mode_rows = persistant_opendal::certify_file_mode(mode_work.path(), &env)
        .await
        .expect("file mode test ran");
    let mode_tmp = mode_work.path().to_string_lossy().into_owned();
    rows.extend(clean(mode_rows, &[mode_tmp.as_str()]));

    // `storage.no_plaintext_at_rest`: a canary written through every operation, searched raw.
    let plain_work = tempfile::tempdir().unwrap();
    let plain_rows = persistant_opendal::certify_no_plaintext_at_rest(plain_work.path(), &env)
        .await
        .expect("plaintext test ran");
    let plain_tmp = plain_work.path().to_string_lossy().into_owned();
    rows.extend(clean(plain_rows, &[plain_tmp.as_str()]));

    // `blob.delete_erased`: the file read back through a second hard link after the delete.
    let erase_work = tempfile::tempdir().unwrap();
    let erase_rows = persistant_opendal::certify_delete_erased(erase_work.path(), &env)
        .await
        .expect("delete_erased test ran");
    let erase_tmp = erase_work.path().to_string_lossy().into_owned();
    rows.extend(clean(erase_rows, &[erase_tmp.as_str()]));

    // KvOnBlob over each, strict; plus the explicit in-place mode on fs without a scratch dir.
    let kvs = [
        KvOnBlob::new(OpendalBlob::memory().unwrap()),
        KvOnBlob::new(OpendalBlob::fs(root_c.path(), None).unwrap()),
        KvOnBlob::with_in_place_put(OpendalBlob::fs(root_c.path(), None).unwrap()),
        KvOnBlob::new(OpendalBlob::fs(root_d.path(), Some(scratch_d.path())).unwrap()),
    ];
    for kv in &kvs {
        let cfg = merged(kv.blob().config(), &kv.config());
        let certified = persistant_kv::conformance::certify(kv, &cfg, &env).await;
        rows.extend(clean(certified.into_iter().map(|c| c.row), &tmp));
        // `text.index`: the text index over this kv (a blob store has no atomic batch: off, missing).
        let text =
            persistant_kv::text_conformance::certify(kv, kv.backend(), kv.version(), &cfg, &env)
                .await;
        rows.extend(clean([text], &tmp));
    }

    // `kv.delete_erased` over the same four configurations, each on a root of its own so the
    // hard links see only this trial's files.
    let kv_work = tempfile::tempdir().unwrap();
    let mem_kv = KvOnBlob::new(OpendalBlob::memory().unwrap());
    let mut erase_kvs: Vec<(KvOnBlob<OpendalBlob>, Option<std::path::PathBuf>)> =
        vec![(mem_kv, None)];
    for (i, (scratch, in_place)) in [(false, false), (false, true), (true, false)]
        .into_iter()
        .enumerate()
    {
        let root = kv_work.path().join(format!("root{i}"));
        std::fs::create_dir_all(&root).unwrap();
        let scratch_dir = kv_work.path().join(format!("scratch{i}"));
        let blob = OpendalBlob::fs(&root, scratch.then_some(scratch_dir.as_path())).unwrap();
        let kv = if in_place {
            KvOnBlob::with_in_place_put(blob)
        } else {
            KvOnBlob::new(blob)
        };
        erase_kvs.push((kv, Some(root)));
    }
    let kv_tmp = kv_work.path().to_string_lossy().into_owned();
    for (i, (kv, root)) in erase_kvs.iter().enumerate() {
        let t = kv_erased_trial(kv, root.as_deref(), &kv_work.path().join(format!("a{i}"))).await;
        let cfg = merged(kv.blob().config(), &kv.config());
        let row = persistant_opendal::erased_row(
            "kv.delete_erased",
            kv.backend(),
            kv.version(),
            cfg,
            &env,
            "delete_erased::kv.delete_erased",
            &t,
        );
        rows.extend(clean([row], &[kv_tmp.as_str()]));
    }

    // Keep the example honest about what it did: every row names its backend.
    assert!(rows.iter().all(|r| !r.backend.is_empty()));
    let _ = fs_unset.backend();
    print!("{}", Matrix::from_rows(rows).to_tsv());
}

/// As the blob trial, one layer up: the record file of a key, read back through a second link.
async fn kv_erased_trial(
    kv: &KvOnBlob<OpendalBlob>,
    root: Option<&Path>,
    work: &Path,
) -> ErasedTrial {
    let canary = persistant_opendal::ERASED_CANARY.to_vec();
    let mut t = ErasedTrial::default();
    for key in ["user/erased", "user/plain"] {
        if let Err(e) = kv.put(key, canary.clone(), None).await {
            t.unsupported = Some(format!("needs kv.put, which this configuration lacks: {e}"));
            return t;
        }
    }
    if let Some(root) = root {
        persistant_opendal::link_all(root, work, "f").unwrap();
        t.positive = persistant_opendal::erased_hits(work).unwrap().len() == 2;
    }
    match kv.delete_erased("user/erased").await {
        Ok(()) => {}
        Err(persistant_kv::KvError::Unsupported(m)) => {
            t.unsupported = Some(format!("kv does not support: {m}"));
            return t;
        }
        Err(e) => panic!("delete_erased: {e}"),
    }
    kv.delete("user/plain").await.unwrap();
    let gone = kv.get("user/erased").await.unwrap().is_none();
    let left = persistant_opendal::erased_hits(work).unwrap();
    // Linked in blob-key order: the hex of `user/erased` sorts before that of `user/plain`.
    t.control_kept = left.contains(&"f-1".to_string());
    t.erased = gone && !left.contains(&"f-0".to_string());
    t
}
