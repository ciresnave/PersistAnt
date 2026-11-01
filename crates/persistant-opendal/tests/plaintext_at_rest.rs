// SPDX-License-Identifier: MIT OR Apache-2.0
//! `storage.no_plaintext_at_rest` for the `fs` driver: a canary written through every operation is
//! looked for, raw, under the data directory and the scratch directory. The driver stores bytes as
//! given, so the canary is found and the capability is off; the scanner that says so is checked on
//! a file known to hold the canary and on one known not to.

use persistant_core::{Config, Reason};
use persistant_opendal::{certify_no_plaintext_at_rest, find_canary};

const CANARY: &[u8] = b"CANARY-7f3a91c2-not-a-secret";

#[test]
fn the_scanner_finds_a_canary_wherever_it_hides() {
    let dir = tempfile::tempdir().unwrap();
    let deep = dir.path().join("a/b/.hidden");
    std::fs::create_dir_all(&deep).unwrap();
    // In the middle of a larger file, not at its start.
    let mut bytes = vec![b'.'; 70_000];
    bytes.extend_from_slice(CANARY);
    bytes.extend_from_slice(&[b'.'; 10]);
    std::fs::write(deep.join("f.part"), &bytes).unwrap();
    let found = find_canary(dir.path(), CANARY).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].ends_with("f.part"), "{found:?}");
}

#[test]
fn the_scanner_does_not_find_what_is_not_there() {
    let dir = tempfile::tempdir().unwrap();
    // The same bytes, XORed: what an encrypting layer would leave (a control for "says no").
    let masked: Vec<u8> = CANARY.iter().map(|b| b ^ 0x5a).collect();
    std::fs::write(dir.path().join("masked"), &masked).unwrap();
    // A prefix of the canary is not the canary.
    std::fs::write(dir.path().join("prefix"), &CANARY[..CANARY.len() - 1]).unwrap();
    assert!(find_canary(dir.path(), CANARY).unwrap().is_empty());
    assert!(find_canary(dir.path(), b"").is_err(), "an empty canary matches everything");
}

#[tokio::test]
async fn plain_fs_stores_the_canary_raw_so_the_capability_is_off() {
    let work = tempfile::tempdir().unwrap();
    let rows = certify_no_plaintext_at_rest(work.path(), &Config::new())
        .await
        .unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for r in &rows {
        assert!(!r.enabled, "{r:?}");
        assert_eq!(r.reason, Some(Reason::Missing), "{r:?}");
        assert_eq!(r.subject.as_str(), "storage.no_plaintext_at_rest");
        assert!(r.note.contains("found raw"), "{}", r.note);
        assert_eq!(
            r.metadata.to_string().matches("controls=held").count(),
            1,
            "{:?}",
            r.metadata
        );
    }
}
