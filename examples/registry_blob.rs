// SPDX-License-Identifier: MIT OR Apache-2.0
//! The blob store through the registry: what the old `Store` offered beside key-value (`Stat`, a
//! whole-file atomic replace, listing) plus `append`, which it never had. Opened by id like the
//! key-value store; a backend that cannot give one of the operations is not listed for it.
//!
//! `cargo run --example registry_blob --features registry`: `fs` in a temporary directory.

use persistant::blob::BlobBackend;
use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::registry::{self, Endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids = [
        "blob.read",
        "blob.write",
        "blob.stat",
        "blob.list",
        "blob.replace_atomic",
        "blob.append",
    ]
    .map(|s| CapabilityId::new(s).expect("a literal capability id is well formed"));
    let needs = ProviderNeeds::new(ids.to_vec(), Durability::Process);
    // Who qualifies, and why: `fs` does; `memory` is Volatile and `sqlite` has no blob rows.
    for l in registry::providers(&needs)? {
        println!(
            "{} qualifies: {}",
            l.id,
            l.why().first().cloned().unwrap_or_default()
        );
    }

    let dir = tempfile::tempdir()?;
    let handle =
        registry::open_named("fs", &Endpoint::path(dir.path().join("files")), &needs).await?;
    let blob = handle.blob().expect("the needs named blob ids");

    // A journal: append adds whole entries in call order.
    blob.append("journal/today", b"started\n".to_vec()).await?;
    blob.append("journal/today", b"stopped\n".to_vec()).await?;
    // A settings file: replaced whole, a reader never sees half of it.
    blob.replace_atomic("settings", b"mode=fast".to_vec())
        .await?;

    let entry = blob.stat("journal/today").await?.expect("just written");
    println!("{}: {} bytes", entry.key, entry.len);
    let page = blob.list("journal/", None, 100).await?;
    println!("under journal/: {}", page.entries.len());
    assert_eq!(blob.read("settings").await?, Some(b"mode=fast".to_vec()));

    // The key-value store sits in the same directory; its keys are reserved here.
    let refused = blob.write("kvx/anything", b"x".to_vec()).await;
    println!(
        "a reserved key: {}",
        refused.expect_err("kvx/ is the key-value store's")
    );
    Ok(())
}
