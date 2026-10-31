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
use persistant_kv::KvOnBlob;
use persistant_opendal::OpendalBlob;

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
    }

    // Keep the example honest about what it did: every row names its backend.
    assert!(rows.iter().all(|r| !r.backend.is_empty()));
    let _ = fs_unset.backend();
    print!("{}", Matrix::from_rows(rows).to_tsv());
}
