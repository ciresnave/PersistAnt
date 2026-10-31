// SPDX-License-Identifier: MIT OR Apache-2.0
//! Large values through the registry: a streamed write that is published whole or not at all, and a
//! ranged read that fetches part of a value without loading the rest. The same two operations exist
//! on the blocking handle (`persistant::blocking::registry`).
//!
//! `cargo run --example registry_blob_stream --features registry`: `fs` in a temporary directory.

use persistant::blob::BlobBackend;
use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::registry::{self, Endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids = [
        "blob.read",
        "blob.read_range",
        "blob.write_stream",
        "blob.write_stream.atomic",
    ]
    .map(|s| CapabilityId::new(s).expect("a literal capability id is well formed"));
    let needs = ProviderNeeds::new(ids.to_vec(), Durability::Process);
    let dir = tempfile::tempdir()?;
    let handle =
        registry::open_named("fs", &Endpoint::path(dir.path().join("files")), &needs).await?;
    let blob = handle.blob().expect("the needs named blob ids");

    // An upload in chunks: a reader sees the whole old value or the whole new one, never a mix.
    let mut sink = blob.write_stream_atomic("video/intro").await?;
    for chunk in [&b"0123456789"[..], b"abcdefghij", b"ABCDEFGHIJ"] {
        sink.write(chunk).await?;
    }
    println!("before finish: {:?}", blob.stat("video/intro").await?);
    sink.finish().await?;

    // Seek into it: bytes 10..15 only.
    let part = blob.read_range("video/intro", 10, 5).await?;
    println!("range 10..15: {}", String::from_utf8_lossy(&part.unwrap()));

    // Changing your mind leaves the key as it was.
    let mut sink = blob.write_stream("video/intro").await?;
    sink.write(b"partial").await?;
    sink.abort().await?;
    println!(
        "length after abort: {}",
        blob.stat("video/intro").await?.unwrap().len
    );

    // The key-value store's names are refused here as on every other blob operation.
    let refused = blob.write_stream("kvx/anything").await.err();
    println!(
        "a reserved key: {}",
        refused.expect("kvx/ is the key-value store's")
    );
    Ok(())
}
