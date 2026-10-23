// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opening a store through the registry instead of naming a backend in code.
//!
//! Before (the `Store` + `Config` API of persistant 0.4): the backend is a line of code.
//!
//! ```ignore
//! let needs = Needs::new().with(Need::Read).with(Need::Write).with(Need::AtomicReplace);
//! let store = Store::open(Config::Fs { root: dir, atomic_write_dir: scratch }, needs)?;
//! ```
//!
//! After: the program states what it needs by capability id, asks which backends qualify, stores the
//! operator's choice (an id and an endpoint) in its own configuration, and opens that. A backend
//! PersistAnt gains later reaches the program on its next rebuild against a newer `persistant`.
//!
//! `cargo run --example registry_instead_of_config --features registry -- [id [path]]`: default `fs` in a temporary
//! directory. `path` is a directory for `fs` and a database file for `sqlite`; the `sqlite`, `sqlite-full`, `postgres`,
//! `mysql` and `mariadb` ids exist only in a build with `registry-sqlite` (or `registry-postgres`, `registry-mysql`).
//!
//! The example is safe on a store that already holds data: every key it writes is under a prefix of its own (process id and
//! time), it reads back only that prefix, and it deletes only what it wrote. See
//! `docs/REGISTRY-FOR-STORE-USERS.md` for the mapping from `Need` to capability ids.

use persistant::capability::{CapabilityId, choose::Durability, provider::ProviderNeeds};
use persistant::kv::KvBackend;
use persistant::registry::{self, Endpoint};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id = |s: &str| CapabilityId::new(s).expect("a literal capability id is well formed");

    // 1. What the program needs. Read, Write and Delete are get, put and delete; List is
    //    prefix_list. AtomicReplace has no id of its own: every backend the registry lists replaces a
    //    value whole. CreateIfAbsent is `kv.put_if_absent`, which the registry's `fs` does not offer
    //    (asking for it lists only the SQL backends); see the guide.
    let needs = ProviderNeeds::new(
        vec![
            id("kv.get"),
            id("kv.put"),
            id("kv.delete"),
            id("kv.prefix_list"),
        ],
        Durability::Process,
    );

    // 2. What qualifies, best first: the list a setup screen shows the operator.
    let list = registry::providers(&needs)?;
    for l in &list {
        println!("{:<12} {}", l.id, l.title);
    }

    // 3. The operator's choice, as the program would have read it from its own configuration.
    let mut args = std::env::args().skip(1);
    let chosen = args.next().unwrap_or_else(|| "fs".to_string());
    let scratch = tempfile::tempdir()?;
    let dir = args
        .next()
        .map_or_else(|| scratch.path().join("state"), std::path::PathBuf::from);

    // 4. Open it. The gate is checked again here: a backend that no longer meets the needs is refused.
    let handle = registry::open_named(&chosen, &Endpoint::path(&dir), &needs).await?;
    let kv = handle.kv();

    // 5. Backend-agnostic calls only, under a prefix that no other run and no existing data shares.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let prefix = format!("registry-example/{}-{nanos}/", std::process::id());
    let (a, b) = (format!("{prefix}a"), format!("{prefix}b"));
    kv.put(&a, b"one".to_vec(), None).await?;
    kv.put(&b, b"two".to_vec(), None).await?;
    let keys = kv.prefix_list(&prefix, None, 100).await?.keys;
    assert_eq!(
        keys.len(),
        2,
        "our prefix holds exactly what this run wrote"
    );
    println!("{} keys under {prefix} on {}", keys.len(), handle.id());
    // Remove only what was written here.
    kv.delete(&a).await?;
    kv.delete(&b).await?;
    assert_eq!(kv.get(&a).await?, None);
    assert!(kv.prefix_list(&prefix, None, 100).await?.keys.is_empty());
    Ok(())
}
