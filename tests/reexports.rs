// SPDX-License-Identifier: MIT OR Apache-2.0
//! The opt-in re-exports are reachable under the documented names and work. Each test is compiled
//! only with its feature; `cargo test -p persistant --all-features` runs them all.

#[cfg(feature = "core")]
#[test]
fn capability_reads_a_matrix_and_validates_ids() {
    use persistant::capability::{CapabilityId, Config, Lookup, Matrix};
    assert!(CapabilityId::new("Not An Id").is_err());
    let text = "subject_kind\tsubject\tbackend\tversion\tconfig\tenabled\treason\tmetadata\tnote\n\
                capability\tkv.get\tb\t1\t\tyes\t\t\t\n";
    let m = Matrix::parse(text).unwrap();
    let id = CapabilityId::new("kv.get").unwrap();
    assert_eq!(m.lookup(&id, "b", "1", &Config::new()), Lookup::On);
    assert_eq!(m.lookup(&id, "b", "2", &Config::new()), Lookup::NoRow);
}

#[cfg(feature = "blob")]
#[tokio::test]
async fn blob_memory_backend_round_trips() {
    use persistant::blob::{BlobBackend, MemoryBlob};
    let b = MemoryBlob::new();
    b.write("k", b"v".to_vec()).await.unwrap();
    assert_eq!(b.read("k").await.unwrap().as_deref(), Some(&b"v"[..]));
}

#[cfg(feature = "kv")]
#[tokio::test]
async fn kv_memory_backend_round_trips() {
    use persistant::kv::{KvBackend, MemoryKv};
    let kv = MemoryKv::new();
    kv.put("k", b"v".to_vec(), None).await.unwrap();
    assert_eq!(kv.get("k").await.unwrap().as_deref(), Some(&b"v"[..]));
}

#[cfg(feature = "relational")]
#[test]
fn relational_names_dialects() {
    assert_eq!(
        persistant::relational::Dialect::Postgres.to_string(),
        "postgres"
    );
}

#[cfg(feature = "opendal-driver")]
#[tokio::test]
async fn opendal_driver_memory_service_works_next_to_the_store() {
    use persistant::blob::BlobBackend;
    let b = persistant::opendal_driver::OpendalBlob::memory().unwrap();
    b.write("k", b"v".to_vec()).await.unwrap();
    assert_eq!(b.read("k").await.unwrap().as_deref(), Some(&b"v"[..]));
}

#[cfg(feature = "sqlx-driver")]
#[test]
fn sqlx_driver_type_is_reachable() {
    // Compile-time reachability only: a connection needs a server (see persistant-sqlx's tests).
    fn _name(_: Option<persistant::sqlx_driver::PgBackend>) {}
}
