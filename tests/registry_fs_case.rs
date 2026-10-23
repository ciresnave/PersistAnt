// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry's `fs` must never let two keys that differ only by case share a value. On a
//! case-insensitive filesystem (NTFS and APFS by default) a key is a file path, so `Key` and `key` are
//! one file: opening `fs` there is refused, and where it is allowed the keys stay apart.
#![cfg(feature = "registry")]

use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::kv::KvBackend;
use persistant::registry::{self, Endpoint, OpenError};

fn needs() -> ProviderNeeds {
    ProviderNeeds::new(
        ["kv.get", "kv.put"]
            .iter()
            .map(|s| CapabilityId::new(*s).expect("a literal id"))
            .collect(),
        Durability::Process,
    )
}

#[tokio::test]
async fn keys_that_differ_only_by_case_never_share_a_value() {
    let dir = tempfile::tempdir().unwrap();
    match registry::open_named("fs", &Endpoint::path(dir.path().join("s")), &needs()).await {
        // Refused: correct wherever the filesystem ignores case, and it says why.
        Err(e) => {
            assert!(
                matches!(e, OpenError::CaseInsensitiveFilesystem { id: "fs", .. }),
                "{e:?}"
            );
            assert!(e.to_string().contains("ignores case"), "{e}");
            // The positive control: this machine really is case-insensitive.
            #[cfg(not(any(windows, target_os = "macos")))]
            panic!("refused on a platform whose default filesystem keeps case: {e}");
        }
        // Opened: the two keys must be two values.
        Ok(h) => {
            let kv = h.kv();
            kv.put("Key", b"upper".to_vec(), None).await.unwrap();
            kv.put("key", b"lower".to_vec(), None).await.unwrap();
            assert_eq!(kv.get("Key").await.unwrap().as_deref(), Some(&b"upper"[..]));
            assert_eq!(kv.get("key").await.unwrap().as_deref(), Some(&b"lower"[..]));
        }
    }
}

#[tokio::test]
async fn the_other_ids_are_not_probed() {
    // `memory` has no directory; the control that the probe belongs to `fs` only.
    let needs = ProviderNeeds::new(
        ["kv.get", "kv.put"]
            .iter()
            .map(|s| CapabilityId::new(*s).expect("a literal id"))
            .collect(),
        Durability::Volatile,
    );
    let h = registry::open_named("memory", &Endpoint::None, &needs)
        .await
        .expect("memory opens everywhere");
    h.kv().put("Key", b"a".to_vec(), None).await.unwrap();
    h.kv().put("key", b"b".to_vec(), None).await.unwrap();
    assert_eq!(h.kv().get("Key").await.unwrap().as_deref(), Some(&b"a"[..]));
}
