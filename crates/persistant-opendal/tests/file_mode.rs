// SPDX-License-Identifier: MIT OR Apache-2.0
//! `OpendalBlob::owner_only()`: every file the `fs` driver creates, by any operation, can be used
//! by its creator alone, whatever the umask; a file that was already there keeps its mode; the plain
//! driver is not claimed to be owner-only. Read back by `owner_only_reading` (mode bits on Unix,
//! `icacls` on Windows), with controls that it can tell open from closed.

use persistant_core::Config;
use persistant_opendal::{OpendalBlob, certify_file_mode, owner_only_reading};

#[tokio::test]
async fn the_probe_finds_both_configurations_on_and_its_controls_hold() {
    let work = tempfile::tempdir().unwrap();
    let rows = certify_file_mode(work.path(), &Config::new())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert!(r.enabled, "{r:?}");
        assert!(
            r.config.to_string().contains("file_mode=owner_only"),
            "{:?}",
            r.config
        );
    }
}

#[tokio::test]
async fn the_reader_calls_a_widened_file_open() {
    // The control by itself: a mutated driver (no owner-only) is caught only if the reader can say no.
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain");
    std::fs::write(&plain, b"x").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(!owner_only_reading(&plain).unwrap());
}

#[tokio::test]
async fn the_plain_driver_does_not_record_the_fact() {
    let root = tempfile::tempdir().unwrap();
    let b = OpendalBlob::fs(root.path(), None).unwrap();
    assert!(!b.config().to_string().contains("file_mode"));
    let b = b.owner_only();
    assert!(b.config().to_string().contains("file_mode=owner_only"));
    // The memory service has no files: asking changes nothing it could be held to.
    let m = OpendalBlob::memory().unwrap().owner_only();
    assert!(!m.config().to_string().contains("file_mode"));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn an_existing_file_keeps_its_mode_and_a_new_directory_is_0700() {
        let root = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let existing = root.path().join("shared");
        std::fs::write(&existing, b"old").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
        let b = OpendalBlob::fs(root.path(), Some(scratch.path()))
            .unwrap()
            .owner_only();
        b.append("shared", b"+".to_vec()).await.unwrap();
        assert_eq!(mode(&existing), 0o640, "append keeps the owner's mode");
        b.create_exclusive("fresh/deep/k", b"v".to_vec())
            .await
            .unwrap();
        assert_eq!(mode(&root.path().join("fresh")), 0o700);
        assert_eq!(mode(&root.path().join("fresh/deep")), 0o700);
        assert_eq!(mode(&root.path().join("fresh/deep/k")), 0o600);
    }

    #[tokio::test]
    async fn a_failed_write_does_not_leave_the_empty_owner_only_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let b = OpendalBlob::fs(root.path(), None).unwrap().owner_only();
        // A key whose parent is a file: the write fails, and no key appears.
        std::fs::write(root.path().join("f"), b"x").unwrap();
        assert!(b.write("f/child", b"v".to_vec()).await.is_err());
        assert!(!root.path().join("f/child").exists());
    }
}
