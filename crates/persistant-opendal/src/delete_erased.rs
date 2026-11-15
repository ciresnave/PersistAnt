// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `blob.delete_erased` test (and the pieces the `kv.delete_erased` test shares): after an
//! erased delete, the bytes the file held are no longer readable.
//!
//! The reading is a second hard link to the same file, made before the delete. Unlinking a key
//! leaves the link's bytes alone, so after a plain `delete` the canary is still readable through the
//! link (the known-bad control: the reading can say "kept"), and after `delete_erased` it must not
//! be. The positive control is that the link did show the canary before any delete.
//!
//! The promise is "the file's contents were overwritten before the name was removed". **Not
//! covered:** the file's freed blocks read from the raw device, and copies a journaling or
//! copy-on-write file system, wear levelling, a snapshot or a backup keeps.

use crate::{OpendalBlob, find_canary};
use persistant_blob::{BlobBackend, BlobError};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::io;
use std::path::Path;

/// The value written through the driver. Unlikely to occur by chance; not a secret.
pub const CANARY: &[u8] = b"CANARY-4d7b20e9-not-a-secret";

/// What one trial found.
#[derive(Debug, Clone, Default)]
pub struct ErasedTrial {
    /// The backend said it cannot (`Unsupported`), with its words.
    pub unsupported: Option<String>,
    /// Before any delete, the second link showed the canary (the positive control).
    pub positive: bool,
    /// After a plain delete, the second link still showed the canary (the known-bad control).
    pub control_kept: bool,
    /// After the erased delete the key was gone and its second link showed no canary.
    pub erased: bool,
}

/// Link every file under `from` into `to`, as `<prefix>-<n>`, returning how many.
pub fn link_all(from: &Path, to: &Path, prefix: &str) -> io::Result<usize> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> io::Result<()> {
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.file_type()?.is_dir() {
                walk(&e.path(), out)?;
            } else {
                out.push(e.path());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(from, &mut files)?;
    files.sort();
    std::fs::create_dir_all(to)?;
    for (i, f) in files.iter().enumerate() {
        std::fs::hard_link(f, to.join(format!("{prefix}-{i}")))?;
    }
    Ok(files.len())
}

/// The names under `dir` that hold the canary.
pub fn hits(dir: &Path) -> io::Result<Vec<String>> {
    Ok(find_canary(dir, CANARY)?
        .into_iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect())
}

/// One matrix row from a trial. `Exact` only with both controls; an `Unsupported` is `Missing`.
pub fn erased_row(
    id: &str,
    backend: &str,
    version: &str,
    config: Config,
    env: &Config,
    test: &str,
    t: &ErasedTrial,
) -> Row {
    let id = CapabilityId::new(id).expect("valid id");
    let controls = Controls {
        positive_passed: t.positive,
        known_bad_failed: t.control_kept,
    };
    let verdict = if t.unsupported.is_some() {
        Verdict::Missing
    } else if t.erased {
        Verdict::Exact
    } else {
        Verdict::Wrong
    };
    let metadata = env.clone().with("test", test).with(
        "controls",
        if t.unsupported.is_some() {
            "not-run"
        } else if controls.positive_passed && controls.known_bad_failed {
            "held"
        } else {
            "failed"
        },
    );
    match verdict.into_row(
        id.clone(),
        backend,
        version,
        config.clone(),
        metadata.clone(),
        controls,
    ) {
        Ok(mut row) => {
            if let Some(m) = &t.unsupported {
                row.note = m.clone();
            }
            row
        }
        Err(_) => Row {
            subject_kind: "capability".to_string(),
            subject: id,
            backend: backend.to_string(),
            version: version.to_string(),
            config,
            enabled: false,
            reason: Some(Reason::Untested),
            metadata,
            note: format!("controls did not hold: {controls:?}"),
            extra: Default::default(),
        },
    }
}

/// Run the blob trial on `b`, whose files (if any) are under `root`; links go under `work/alias`.
async fn blob_trial(b: &OpendalBlob, root: Option<&Path>, work: &Path) -> io::Result<ErasedTrial> {
    let e = |x: BlobError| io::Error::other(x.to_string());
    b.write("k/erased", CANARY.to_vec()).await.map_err(e)?;
    b.write("k/plain", CANARY.to_vec()).await.map_err(e)?;
    let mut t = ErasedTrial::default();
    let alias = work.join("alias");
    if let Some(root) = root {
        link_all(root, &alias, "f")?;
        t.positive = hits(&alias)?.len() == 2;
    }
    match b.delete_erased("k/erased").await {
        Ok(()) => {}
        Err(BlobError::Unsupported(m)) => {
            t.unsupported = Some(format!("blob does not support: {m}"));
            return Ok(t);
        }
        Err(x) => return Err(e(x)),
    }
    b.delete("k/plain").await.map_err(e)?;
    let gone = b.read("k/erased").await.map_err(e)?.is_none();
    let left = hits(&alias)?;
    // Which link is which: the files were linked in key order, `k/erased` then `k/plain`.
    t.control_kept = left.contains(&"f-1".to_string());
    t.erased = gone && !left.contains(&"f-0".to_string());
    Ok(t)
}

/// Certify `blob.delete_erased` for `opendal-fs`, with and without `atomic_write_dir`, and
/// `opendal-memory` (which has no file to overwrite and says so): one row per configuration.
/// `work` is an empty directory the test may fill.
pub async fn certify_delete_erased(work: &Path, env: &Config) -> io::Result<Vec<Row>> {
    let mut rows = Vec::new();
    let mem = OpendalBlob::memory().map_err(io::Error::other)?;
    let t = blob_trial(&mem, None, work).await?;
    rows.push(erased_row(
        "blob.delete_erased",
        "opendal-memory",
        opendal::raw::VERSION,
        Config::new(),
        env,
        "delete_erased::blob.delete_erased",
        &t,
    ));
    for set in [false, true] {
        let tag = if set { "set" } else { "unset" };
        let root = work.join(format!("{tag}-root"));
        let scratch = work.join(format!("{tag}-scratch"));
        std::fs::create_dir_all(&root)?;
        let b =
            OpendalBlob::fs(&root, set.then_some(scratch.as_path())).map_err(io::Error::other)?;
        let t = blob_trial(&b, Some(&root), &work.join(tag)).await?;
        rows.push(erased_row(
            "blob.delete_erased",
            "opendal-fs",
            opendal::raw::VERSION,
            Config::new().with("atomic_write_dir", tag),
            env,
            "delete_erased::blob.delete_erased",
            &t,
        ));
    }
    Ok(rows)
}
