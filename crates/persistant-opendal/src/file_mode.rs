// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `blob.file_mode` test: every file an owner-only `fs` driver creates, by every operation that
//! creates one, can be used by its creator alone.
//!
//! The reading is a different route from the one that sets the mode. On Unix it is the file's mode
//! bits from `stat` (set by `open(2)` with `0600`); on Windows it is the ACL as `icacls` prints it
//! (set by the Win32 security API). Controls: the reader flags a file made group- and
//! world-readable on purpose (the known-bad), and finds an owner-only file owner-only (the
//! positive), so a reader that says yes to everything or no to everything cannot pass.

use crate::OpendalBlob;
use persistant_blob::BlobBackend;
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::io;
use std::path::{Path, PathBuf};

/// Whether `path` can be used by its creator alone, read the way the platform records it.
pub fn owner_only_reading(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(std::fs::metadata(path)?.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        let run = |cmd: &str, args: &[&str]| -> io::Result<String> {
            let out = std::process::Command::new(cmd).args(args).output()?;
            if !out.status.success() {
                return Err(io::Error::other(format!("{cmd} failed: {out:?}")));
            }
            Ok(String::from_utf8_lossy(&out.stdout).to_string())
        };
        let me = run("whoami", &[])?.trim().to_lowercase();
        let text = run("icacls", &[&path.to_string_lossy()])?;
        let mut entries = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("Successfully processed") {
                continue;
            }
            let entry = if i == 0 {
                line.strip_prefix(&*path.to_string_lossy())
                    .unwrap_or(line)
                    .trim()
            } else {
                line
            };
            if entry.contains(":(") {
                entries.push(entry.to_lowercase());
            }
        }
        Ok(entries.len() == 1 && entries[0].starts_with(&format!("{me}:")))
    }
}

/// Every file under `dir` (and, on Unix, every directory under it), recursively.
fn walk(dir: &Path, out: &mut Vec<(PathBuf, bool)>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            // Windows directories inherit their parent's ACL by design (documented).
            if cfg!(unix) {
                out.push((path.clone(), true));
            }
            walk(&path, out)?;
        } else {
            out.push((path, false));
        }
    }
    Ok(())
}

/// Run every file-creating operation on an owner-only driver and read each created path back.
/// Returns the paths that are not owner-only, and how many were read.
async fn trial(root: &Path, scratch: Option<&Path>) -> io::Result<(Vec<String>, usize)> {
    let b = OpendalBlob::fs(root, scratch)
        .map_err(io::Error::other)?
        .owner_only();
    let e = |x: persistant_blob::BlobError| io::Error::other(x.to_string());
    b.write("a/b/written", b"one".to_vec()).await.map_err(e)?;
    b.write("a/b/written", b"two".to_vec()).await.map_err(e)?;
    b.create_exclusive("a/c/created", b"x".to_vec())
        .await
        .map_err(e)?;
    b.append("a/d/appended", b"1".to_vec()).await.map_err(e)?;
    b.append("a/d/appended", b"2".to_vec()).await.map_err(e)?;
    if scratch.is_some() {
        b.replace_atomic("a/e/replaced", b"r".to_vec())
            .await
            .map_err(e)?;
        for (name, atomic) in [("a/f/streamed", false), ("a/g/streamed-atomic", true)] {
            let mut sink = if atomic {
                b.write_stream_atomic(name).await
            } else {
                b.write_stream(name).await
            }
            .map_err(e)?;
            sink.write(b"chunk").await.map_err(e)?;
            sink.finish().await.map_err(e)?;
        }
    }
    let mut found = Vec::new();
    walk(root, &mut found)?;
    if let Some(s) = scratch {
        walk(s, &mut found)?;
    }
    let mut bad = Vec::new();
    for (path, _) in &found {
        if !owner_only_reading(path)? {
            bad.push(path.display().to_string());
        }
    }
    Ok((bad, found.len()))
}

/// Certify `blob.file_mode` for the owner-only `fs` driver, with and without `atomic_write_dir`:
/// one row per configuration. `work` is an empty directory the test may fill.
pub async fn certify_file_mode(work: &Path, env: &Config) -> io::Result<Vec<Row>> {
    // Controls, once: a file the reader must call open, and one it must call closed.
    let control = work.join("control");
    std::fs::create_dir_all(&control)?;
    let closed = persistant_core::owner_only::create_new(&control.join("closed"))?;
    drop(closed);
    // The known-bad: group and others may read it (on Windows it inherits the temp directory's ACL).
    let open = control.join("open");
    std::fs::write(&open, b"x")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o644))?;
    }
    let controls = Controls {
        positive_passed: owner_only_reading(&control.join("closed"))?,
        known_bad_failed: !owner_only_reading(&open)?,
    };

    let id = CapabilityId::new("blob.file_mode").expect("valid id");
    let mut rows = Vec::new();
    for set in [false, true] {
        let root = work.join(if set { "set-root" } else { "unset-root" });
        let scratch = work.join(if set { "set-scratch" } else { "unset-scratch" });
        std::fs::create_dir_all(&root)?;
        let (bad, read) = trial(&root, set.then_some(scratch.as_path())).await?;
        let config = Config::new()
            .with("atomic_write_dir", if set { "set" } else { "unset" })
            .with("file_mode", "owner_only");
        let metadata = env
            .clone()
            .with("test", "file_mode::blob.file_mode")
            .with("paths_read", read.to_string());
        let verdict = if read > 0 && bad.is_empty() {
            Verdict::Exact
        } else {
            Verdict::Wrong
        };
        let backend = "opendal-fs";
        let version = opendal::raw::VERSION;
        rows.push(
            match verdict.into_row(
                id.clone(),
                backend,
                version,
                config.clone(),
                metadata.clone(),
                controls,
            ) {
                Ok(row) => row,
                Err(_) => Row {
                    subject_kind: "capability".to_string(),
                    subject: id.clone(),
                    backend: backend.to_string(),
                    version: version.to_string(),
                    config,
                    enabled: false,
                    reason: Some(if bad.is_empty() {
                        Reason::Untested
                    } else {
                        Reason::Wrong
                    }),
                    metadata,
                    note: if bad.is_empty() {
                        format!("controls did not hold: {controls:?}")
                    } else {
                        format!("not owner-only: {}", bad.join(", "))
                    },
                    extra: Default::default(),
                },
            },
        );
    }
    Ok(rows)
}
