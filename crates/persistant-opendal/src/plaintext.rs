// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `storage.no_plaintext_at_rest` test: a canary written through the layer must not be found as
//! raw bytes in the data directory or the scratch directory.
//!
//! The `fs` driver stores the bytes it is given, so the canary **is** found and the row is off
//! (`missing`, with the files that hold it in the note). That is the honest answer for every
//! plaintext store, and the test exists so that a layer which does encrypt (or an encrypted volume
//! that keeps the files out of reach) can be the first to turn it on. The scanner is checked both
//! ways in `tests/plaintext_at_rest.rs`: it finds a canary in a file known to hold one, and does not
//! find one in a file whose bytes were masked.
//!
//! **Not covered:** the files of a store that is not `fs` (SQLite's journal and WAL are the
//! relational lane's), a canary the layer has split, compressed or encoded before storing (only the
//! raw bytes are searched), and anything outside the two directories given, such as an operating
//! system page file or swap.

use crate::OpendalBlob;
use persistant_blob::BlobBackend;
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::io;
use std::path::{Path, PathBuf};

/// The value written through the driver. Unlikely to occur by chance; not a secret.
const CANARY: &[u8] = b"CANARY-7f3a91c2-not-a-secret";

/// Every file under `dir` (recursively) whose bytes contain `canary`. An empty canary is refused:
/// it would match every file.
pub fn find_canary(dir: &Path, canary: &[u8]) -> io::Result<Vec<PathBuf>> {
    if canary.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an empty canary matches every file",
        ));
    }
    let mut hits = Vec::new();
    scan(dir, canary, &mut hits)?;
    hits.sort();
    Ok(hits)
}

fn scan(dir: &Path, canary: &[u8], hits: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            scan(&path, canary, hits)?;
        } else if std::fs::read(&path)?
            .windows(canary.len())
            .any(|w| w == canary)
        {
            hits.push(path);
        }
    }
    Ok(())
}

fn count_files(dir: &Path) -> io::Result<usize> {
    let mut n = 0;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        n += if entry.file_type()?.is_dir() {
            count_files(&entry.path())?
        } else {
            1
        };
    }
    Ok(n)
}

/// Write the canary through every operation the driver has, then scan `root` and `scratch`.
/// Returns the files that hold it and how many files were read.
async fn trial(root: &Path, scratch: Option<&Path>) -> io::Result<(Vec<PathBuf>, usize)> {
    let b = OpendalBlob::fs(root, scratch).map_err(io::Error::other)?;
    let e = |x: persistant_blob::BlobError| io::Error::other(x.to_string());
    b.write("w/written", CANARY.to_vec()).await.map_err(e)?;
    b.create_exclusive("c/created", CANARY.to_vec())
        .await
        .map_err(e)?;
    b.append("a/appended", CANARY.to_vec()).await.map_err(e)?;
    if scratch.is_some() {
        b.replace_atomic("r/replaced", CANARY.to_vec())
            .await
            .map_err(e)?;
        let mut sink = b.write_stream("s/streamed").await.map_err(e)?;
        sink.write(CANARY).await.map_err(e)?;
        sink.finish().await.map_err(e)?;
    }
    let mut dirs = vec![root];
    dirs.extend(scratch);
    let mut hits = Vec::new();
    let mut read = 0;
    for d in dirs {
        scan(d, CANARY, &mut hits)?;
        read += count_files(d)?;
    }
    hits.sort();
    Ok((hits, read))
}

/// Certify `storage.no_plaintext_at_rest` for the `fs` driver, with and without `atomic_write_dir`:
/// one row per configuration. `work` is an empty directory the test may fill.
pub async fn certify_no_plaintext_at_rest(work: &Path, env: &Config) -> io::Result<Vec<Row>> {
    // Controls, once: the scanner must find a canary in a plain file and must not in a masked one.
    let control = work.join("control");
    std::fs::create_dir_all(&control)?;
    std::fs::write(control.join("plain"), CANARY)?;
    let masked: Vec<u8> = CANARY.iter().map(|b| b ^ 0x5a).collect();
    std::fs::write(control.join("masked"), masked)?;
    let hits = find_canary(&control, CANARY)?;
    // Found in the plain file (the known-bad), not in the masked one (the known-good).
    let held = hits.len() == 1 && hits[0].ends_with("plain");
    let controls = Controls {
        positive_passed: held,
        known_bad_failed: held,
    };

    let id = CapabilityId::new("storage.no_plaintext_at_rest").expect("valid id");
    let mut rows = Vec::new();
    for set in [false, true] {
        let tag = if set { "set" } else { "unset" };
        let root = work.join(format!("{tag}-root"));
        let scratch = work.join(format!("{tag}-scratch"));
        std::fs::create_dir_all(&root)?;
        let (found, read) = trial(&root, set.then_some(scratch.as_path())).await?;
        let config = Config::new().with("atomic_write_dir", tag);
        let metadata = env
            .clone()
            .with("test", "plaintext::storage.no_plaintext_at_rest")
            .with("paths_read", read.to_string())
            .with("controls", if held { "held" } else { "failed" });
        // Found raw: the store keeps plaintext, which is "missing", not a bug. Nothing found with
        // the controls holding is the only `Exact`; this driver never reaches it, and a layer
        // that does must show the controls too.
        let verdict = if found.is_empty() && read > 0 {
            Verdict::Exact
        } else {
            Verdict::Missing
        };
        let mut row = match verdict.into_row(
            id.clone(),
            "opendal-fs",
            opendal::raw::VERSION,
            config.clone(),
            metadata.clone(),
            controls,
        ) {
            Ok(row) => row,
            Err(_) => Row {
                subject_kind: "capability".to_string(),
                subject: id.clone(),
                backend: "opendal-fs".to_string(),
                version: opendal::raw::VERSION.to_string(),
                config,
                enabled: false,
                reason: Some(Reason::Untested),
                metadata,
                note: format!("controls did not hold: {controls:?}"),
                extra: Default::default(),
            },
        };
        if !row.enabled && row.reason == Some(Reason::Missing) {
            let names: Vec<String> = found
                .iter()
                .filter_map(|p| {
                    p.strip_prefix(work)
                        .ok()
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                })
                .collect();
            row.note = format!(
                "canary found raw in {} of {read} files: {}",
                names.len(),
                names.join(", ")
            );
        }
        rows.push(row);
    }
    Ok(rows)
}
