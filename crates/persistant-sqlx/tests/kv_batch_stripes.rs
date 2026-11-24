// SPDX-License-Identifier: MIT OR Apache-2.0
//! ROADMAP row 47: batch locking on PostgreSQL (`bench/pg-store-lock-design.md`).
//!
//! The pair that makes removal of the global lock SAFE and USEFUL: same-key and overlapping-key workers
//! lose no update and never deadlock (safe), and a batch on other keys is not held up by a held lock (useful).
//! The benchmark in `bench/pg-store-lock-variant.txt` priced the lock with workers on DIFFERENT keys; it
//! could not show safety, these tests are what do.
//!
//! Needs `PERSISTANT_TEST_DATABASE_URL`; without it they print a note and pass unless
//! `PERSISTANT_REQUIRE_DATABASE=1` (as `kv_on_postgres.rs`).

use persistant_kv::{BatchOp, KvBackend, KvOnRelational, batch_stripe};
use persistant_relational::{Isolation, RelationalBackend, RelationalTx};
use persistant_sqlx::PgBackend;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One test at a time on the shared server (they share `persistant_kv` and its stripe rows).
static SERVER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn store() -> Option<KvOnRelational<PgBackend>> {
    match std::env::var("PERSISTANT_TEST_DATABASE_URL") {
        Ok(url) => {
            let pg = PgBackend::connect(&url).await.expect("connect");
            Some(KvOnRelational::open(pg).await.expect("open"))
        }
        Err(_) => {
            assert!(
                std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
                "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
            );
            eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
            None
        }
    }
}

/// A key under `prefix` whose stripe satisfies `ok`, and that is not already in `taken`.
fn key_where(prefix: &str, taken: &[String], ok: impl Fn(i64) -> bool) -> String {
    (0..100_000)
        .map(|i| format!("{prefix}/k{i}"))
        .find(|k| !taken.contains(k) && ok(batch_stripe(k)))
        .expect("a key with the wanted stripe")
}

fn num(v: Option<Vec<u8>>) -> u64 {
    v.map_or(0, |b| String::from_utf8(b).unwrap().parse().unwrap())
}

fn prefix(test: &str) -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("stripes/{test}/{t}")
}

/// Known answers, so two processes (and two versions) agree which stripe guards a key.
#[test]
fn stripe_is_a_fixed_hash_in_range() {
    assert_eq!(
        batch_stripe(""),
        1 + (0xcbf2_9ce4_8422_2325_u64 % 256) as i64
    );
    for k in ["a", "tok:tok-1", "tokacc:access-000001", "ünï"] {
        let s = batch_stripe(k);
        assert!((1..=256).contains(&s), "{k} -> {s}");
        assert_eq!(s, batch_stripe(k));
    }
    // FNV-1a 64 of "a" is af63dc4c8601ec8c.
    assert_eq!(
        batch_stripe("a"),
        1 + (0xaf63_dc4c_8601_ec8c_u64 % 256) as i64
    );
}

/// USEFUL: hold the lock row that guards key A. A batch on a key in another stripe must finish;
/// a batch on a different key in A's stripe must still wait (the control: the lock holds what it should),
/// and finishes once released. With one global lock row this fails (the row held is not the row taken).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disjoint_stripes_run_while_a_stripe_is_held() {
    let Some(kv) = store().await else { return };
    let _s = SERVER.lock().await;
    let p = prefix("disjoint");
    let a = key_where(&p, &[], |s| s != 1);
    let sa = batch_stripe(&a);
    let b = key_where(&p, std::slice::from_ref(&a), |s| s != sa && s != 1);
    let same = key_where(&p, &[a.clone(), b.clone()], |s| s == sa);
    let lock = format!("SELECT id FROM persistant_kv_batch WHERE id = {sa} FOR UPDATE");
    // Hold stripe A's row in an open transaction of our own.
    let mut holder = kv
        .backend_handle()
        .begin(Isolation::ReadCommitted)
        .await
        .unwrap();
    holder.query(&lock, &[]).await.unwrap();
    let t = Instant::now();
    let other = tokio::time::timeout(
        Duration::from_secs(5),
        kv.batch(&[BatchOp::put(&b, b"b".to_vec())]),
    )
    .await;
    let other_done = matches!(other, Ok(Ok(true)));
    let took = t.elapsed();
    // Dropped on timeout: a cut-off batch rolls back.
    let same_blocked = tokio::time::timeout(
        Duration::from_millis(800),
        kv.batch(&[BatchOp::put(&same, b"s".to_vec())]),
    )
    .await
    .is_err();
    holder.rollback().await.unwrap();
    assert!(
        other_done,
        "a batch on another stripe did not finish while stripe {sa} was held ({took:?})"
    );
    assert!(
        same_blocked,
        "a batch on stripe {sa} was NOT held by stripe {sa}'s lock"
    );
    assert_eq!(
        kv.get(&same).await.unwrap(),
        None,
        "the cut-off batch must not have applied"
    );
    // Released: the same-stripe batch now goes through.
    assert!(
        kv.batch(&[BatchOp::put(&same, b"s".to_vec())])
            .await
            .unwrap()
    );
    for k in [&a, &b, &same] {
        kv.delete(k).await.unwrap();
    }
}

/// SAFE: 8 workers read-modify-write ONE key. No lost update, no error, no deadlock retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_key_workers_lose_no_update() {
    let Some(kv) = store().await else { return };
    let _s = SERVER.lock().await;
    let kv = Arc::new(kv);
    let key = format!("{}/counter", prefix("samekey"));
    let before = kv.batch_retries();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (kv, key) = (kv.clone(), key.clone());
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                loop {
                    let cur = kv.get(&key).await.unwrap();
                    let next = (num(cur.clone()) + 1).to_string().into_bytes();
                    if kv
                        .batch(&[BatchOp::require(&key, cur), BatchOp::put(&key, next)])
                        .await
                        .expect("a refused racer is told false, never an error")
                    {
                        break;
                    }
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(num(kv.get(&key).await.unwrap()), 80, "lost updates");
    assert_eq!(
        kv.batch_retries() - before,
        0,
        "deadlock or serialisation victims"
    );
    kv.delete(&key).await.unwrap();
}

/// SAFE: 8 workers each update the SAME THREE keys, naming them in a different order per worker, with
/// preconditions on all three. Overlapping key sets, opposite order: the deadlock the global lock was
/// added for. All three counters must reach 64 together and no retry may be needed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn overlapping_keys_in_opposite_order_do_not_deadlock() {
    let Some(kv) = store().await else { return };
    let _s = SERVER.lock().await;
    let kv = Arc::new(kv);
    let p = prefix("overlap");
    // Three keys in three different stripes (so a stripe scheme has to order them).
    let k0 = key_where(&p, &[], |_| true);
    let k1 = key_where(&p, std::slice::from_ref(&k0), |s| s != batch_stripe(&k0));
    let k2 = key_where(&p, &[k0.clone(), k1.clone()], |s| {
        s != batch_stripe(&k0) && s != batch_stripe(&k1)
    });
    let keys = [k0, k1, k2];
    let before = kv.batch_retries();
    let mut tasks = Vec::new();
    for w in 0..8usize {
        let (kv, keys) = (kv.clone(), keys.clone());
        tasks.push(tokio::spawn(async move {
            for _ in 0..8 {
                loop {
                    let mut cur = Vec::new();
                    for k in &keys {
                        cur.push(kv.get(k).await.unwrap());
                    }
                    let mut ops = Vec::new();
                    for step in 0..3 {
                        // Worker w starts at key (w % 3) and walks round: three distinct orders, and the
                        // reverse for odd workers.
                        let i = if w % 2 == 0 {
                            (w + step) % 3
                        } else {
                            (w + 3 - step % 3) % 3
                        };
                        ops.push(BatchOp::require(&keys[i], cur[i].clone()));
                    }
                    for step in 0..3 {
                        let i = if w % 2 == 0 {
                            (w + step) % 3
                        } else {
                            (w + 3 - step % 3) % 3
                        };
                        let next = (num(cur[i].clone()) + 1).to_string().into_bytes();
                        ops.push(BatchOp::put(&keys[i], next));
                    }
                    if kv.batch(&ops).await.expect("never an error") {
                        break;
                    }
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    for k in &keys {
        assert_eq!(num(kv.get(k).await.unwrap()), 64, "lost update on {k}");
    }
    assert_eq!(
        kv.batch_retries() - before,
        0,
        "deadlock or serialisation victims"
    );
    for k in &keys {
        kv.delete(k).await.unwrap();
    }
}

/// SAFE: two batches require two ABSENT keys and write both, in opposite order, 200 rounds on fresh keys.
/// Exactly one wins each round, the other is told `false`, no error, no retry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn absent_keys_in_opposite_order_one_winner_no_retry() {
    let Some(kv) = store().await else { return };
    let _s = SERVER.lock().await;
    let kv = Arc::new(kv);
    let p = prefix("absent");
    let before = kv.batch_retries();
    for round in 0..200 {
        let (a, b) = (format!("{p}/r{round}a"), format!("{p}/r{round}b"));
        let ab = vec![
            BatchOp::require(&a, None),
            BatchOp::require(&b, None),
            BatchOp::put(&a, b"1".to_vec()),
            BatchOp::put(&b, b"1".to_vec()),
        ];
        let ba = vec![
            BatchOp::require(&b, None),
            BatchOp::require(&a, None),
            BatchOp::put(&b, b"2".to_vec()),
            BatchOp::put(&a, b"2".to_vec()),
        ];
        let (k1, k2) = (kv.clone(), kv.clone());
        let t1 = tokio::spawn(async move { k1.batch(&ab).await.expect("never an error") });
        let t2 = tokio::spawn(async move { k2.batch(&ba).await.expect("never an error") });
        let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
        assert!(
            r1 ^ r2,
            "round {round}: exactly one must win, got {r1} and {r2}"
        );
        let (va, vb) = (kv.get(&a).await.unwrap(), kv.get(&b).await.unwrap());
        assert_eq!(
            va, vb,
            "round {round}: the winner's two writes must be together, got {va:?} {vb:?}"
        );
        kv.delete(&a).await.unwrap();
        kv.delete(&b).await.unwrap();
    }
    assert_eq!(
        kv.batch_retries() - before,
        0,
        "deadlock or serialisation victims"
    );
}

/// SAFE: a batch refused mid-way (a later precondition fails) leaves none of its earlier writes, and a
/// batch cut off while waiting for a held lock leaves none either.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_or_cut_off_batch_leaves_no_partial_rows() {
    let Some(kv) = store().await else { return };
    let _s = SERVER.lock().await;
    let p = prefix("rollback");
    let (x, y, z) = (format!("{p}/x"), format!("{p}/y"), format!("{p}/z"));
    kv.put(&z, b"real".to_vec(), None).await.unwrap();
    let applied = kv
        .batch(&[
            BatchOp::put(&x, b"x".to_vec()),
            BatchOp::put(&y, b"y".to_vec()),
            BatchOp::require(&z, Some(b"wrong".to_vec())),
        ])
        .await
        .unwrap();
    assert!(!applied, "the failed precondition must refuse the batch");
    assert_eq!(
        kv.get(&x).await.unwrap(),
        None,
        "x written by a refused batch"
    );
    assert_eq!(
        kv.get(&y).await.unwrap(),
        None,
        "y written by a refused batch"
    );
    assert_eq!(kv.get(&z).await.unwrap(), Some(b"real".to_vec()));
    // Cut off while queued behind a held stripe lock (the lock of y; x and y may share a stripe or not).
    let lock = format!(
        "SELECT id FROM persistant_kv_batch WHERE id = {} FOR UPDATE",
        batch_stripe(&y)
    );
    let mut holder = kv
        .backend_handle()
        .begin(Isolation::ReadCommitted)
        .await
        .unwrap();
    holder.query(&lock, &[]).await.unwrap();
    let cut = tokio::time::timeout(
        Duration::from_millis(500),
        kv.batch(&[
            BatchOp::put(&x, b"x".to_vec()),
            BatchOp::put(&y, b"y".to_vec()),
        ]),
    )
    .await
    .is_err();
    holder.rollback().await.unwrap();
    assert!(cut, "the batch was not held by y's lock");
    assert_eq!(
        kv.get(&x).await.unwrap(),
        None,
        "x written by a cut-off batch"
    );
    assert_eq!(
        kv.get(&y).await.unwrap(),
        None,
        "y written by a cut-off batch"
    );
    kv.delete(&z).await.unwrap();
}
