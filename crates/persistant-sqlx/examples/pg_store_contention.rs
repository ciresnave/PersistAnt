// SPDX-License-Identifier: MIT OR Apache-2.0
//! ROADMAP row 30 follow-up: why store token costs 10.6x at 8 workers. Design (hypothesis, what refutes it):
//! `bench/pg-store-contention-design.md`, written before this ran.
//!
//! `PERSISTANT_TEST_DATABASE_URL=postgres://... PAIRS=15 cargo run -q --release -p persistant-sqlx --example pg_store_contention`
//!
//! For W in 1,2,4,8 workers and op in store / put_only / control_same / control_slow: interleaved pairs (direct then
//! persistant, then the reverse), 5 warm-up pairs discarded, each worker 25 calls per side, datum = per-pair ratio of
//! time per call. A sampler on its own connection counts `pg_stat_activity` wait events during each side.

use persistant_kv::index::Indexed;
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_relational::RelationalBackend;
use persistant_sqlx::PgBackend;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SEED: usize = 2000;
const BATCH: usize = 25;
const WARM: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Store,
    PutOnly,
    ControlSame,
    ControlSlow,
}

fn tok_access(i: usize) -> String {
    format!("access-{i:06}-{}", "a".repeat(40))
}
fn tok_json(i: usize) -> String {
    format!(
        r#"{{"token_id":"tok-{i}","user_id":"user-{}","access_token":"{}","pad":"{}"}}"#,
        i % 500,
        tok_access(i),
        "x".repeat(480)
    )
}
const INS_TOKEN: &str = "INSERT INTO auth_tokens (token_id, user_id, access_token, refresh_token, token_type, expires_at, scopes, issued_at, auth_method, subject, issuer, client_id, metadata, user_profile, permissions, roles) VALUES ($1, $2, $3, $4, $5, NOW() + interval '1 hour', $6, NOW(), $7, $8, $9, $10, $11::text::jsonb, $12::text::jsonb, $13, $14) ON CONFLICT (token_id) DO UPDATE SET access_token = EXCLUDED.access_token, refresh_token = EXCLUDED.refresh_token, expires_at = EXCLUDED.expires_at, user_profile = EXCLUDED.user_profile, permissions = EXCLUDED.permissions, roles = EXCLUDED.roles";

async fn direct_store_token(pool: &sqlx::PgPool, i: usize) {
    sqlx::query(INS_TOKEN)
        .bind(format!("tok-{i}"))
        .bind(format!("user-{}", i % 500))
        .bind(tok_access(i))
        .bind(Some("refresh-token-value".to_string()))
        .bind("Bearer")
        .bind(vec!["read".to_string(), "write".to_string()])
        .bind("password")
        .bind(Some(format!("user-{}", i % 500)))
        .bind(Some("issuer".to_string()))
        .bind(Some("client".to_string()))
        .bind("{}".to_string())
        .bind(tok_json(i))
        .bind(vec!["a".to_string(), "b".to_string()])
        .bind(vec!["user".to_string()])
        .execute(pool)
        .await
        .unwrap();
}

/// auth-framework's `upsert` on the update path: read the record, then one atomic batch that replaces it.
async fn persist_store_token(kv: &KvOnRelational<PgBackend>, i: usize) {
    let primary = format!("tok:tok-{i}");
    let idx = [
        format!("tokacc:{}", tok_access(i)),
        format!("tokuser:user-{}:tok-{i}", i % 500),
    ];
    let idx_ref = [idx[0].as_str(), idx[1].as_str()];
    let existing = kv.get(&primary).await.unwrap().expect("seeded");
    let ok = Indexed::new(kv)
        .replace(
            &primary,
            &existing,
            tok_json(i).into_bytes(),
            &idx_ref,
            &idx_ref,
            None,
        )
        .await
        .unwrap();
    assert!(ok);
}

async fn worker(
    op: Op,
    direct: bool,
    pool: &sqlx::PgPool,
    kv: &KvOnRelational<PgBackend>,
    base: usize,
) {
    for k in 0..BATCH {
        run_one(op, direct, pool, kv, base + k).await;
    }
}

fn load() -> String {
    let ps = "$p=Get-Process; $c=@($p|?{$_.Name -match '^(cargo|rustc)'}).Count; $k=@($p|?{$_.Name -match 'claude'}).Count; $u=(Get-CimInstance Win32_Processor|Measure-Object LoadPercentage -Average).Average; \"$c $k $u\"";
    let out = Command::new("powershell")
        .args(["-NoProfile", "-Command", ps])
        .output();
    let host = out
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let dock = Command::new("docker")
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{.CPUPerc}}",
            "persistant-bench-pg",
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    format!("{host}\t{dock}")
}

fn pct(v: &[f64], p: f64) -> f64 {
    v[((v.len() - 1) as f64 * p).round() as usize]
}

async fn run_one(
    op: Op,
    direct: bool,
    pool: &sqlx::PgPool,
    kv: &KvOnRelational<PgBackend>,
    i: usize,
) {
    let i = i % SEED;
    match (op, direct) {
        (Op::Store, true) | (Op::PutOnly, true) | (Op::ControlSame, _) | (Op::ControlSlow, _) => {
            if op == Op::ControlSlow && direct {
                sqlx::query("SELECT pg_sleep(0.002)")
                    .execute(pool)
                    .await
                    .unwrap();
            }
            direct_store_token(pool, i).await;
        }
        (Op::Store, false) => persist_store_token(kv, i).await,
        (Op::PutOnly, false) => {
            kv.put(&format!("tok:tok-{i}"), tok_json(i).into_bytes(), None)
                .await
                .unwrap();
        }
    }
}

/// Poll every future to completion on this task (no `futures` crate here).
async fn join_all<'a>(mut fs: Vec<Pin<Box<dyn Future<Output = ()> + 'a>>>) {
    std::future::poll_fn(|cx| {
        fs.retain_mut(|f| f.as_mut().poll(cx).is_pending());
        if fs.is_empty() {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url =
        std::env::var("PERSISTANT_TEST_DATABASE_URL").expect("set PERSISTANT_TEST_DATABASE_URL");
    let pairs: usize = std::env::var("PAIRS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let pg = PgBackend::connect(&url).await.expect("connect");
    let pool = pg.pool().clone();
    for t in ["auth_tokens", "persistant_kv"] {
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {t}")))
            .execute(&pool)
            .await;
    }
    // auth-framework @ 0cec4464, PostgresStorage::migrate (the tokens table).
    for ddl in [
        "CREATE TABLE auth_tokens (token_id VARCHAR(255) PRIMARY KEY, user_id VARCHAR(255) NOT NULL, access_token TEXT NOT NULL UNIQUE, refresh_token TEXT, token_type VARCHAR(50), expires_at TIMESTAMPTZ NOT NULL, scopes TEXT[], issued_at TIMESTAMPTZ NOT NULL, auth_method VARCHAR(100) NOT NULL, subject VARCHAR(255), issuer VARCHAR(255), client_id VARCHAR(255), metadata JSONB, created_at TIMESTAMPTZ DEFAULT NOW(), user_profile JSONB, permissions TEXT[] NOT NULL DEFAULT '{}', roles TEXT[] NOT NULL DEFAULT '{}')",
        "CREATE INDEX idx_auth_tokens_user_id ON auth_tokens (user_id)",
        "CREATE INDEX idx_auth_tokens_access_token ON auth_tokens (access_token)",
        "CREATE INDEX idx_auth_tokens_expires_at ON auth_tokens (expires_at)",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(ddl.to_string()))
            .execute(&pool)
            .await
            .unwrap();
    }
    let kv = KvOnRelational::open(pg.clone()).await.expect("kv open");
    for i in 0..SEED {
        direct_store_token(&pool, i).await;
        let idx = [
            format!("tokacc:{}", tok_access(i)),
            format!("tokuser:user-{}:tok-{i}", i % 500),
        ];
        assert!(
            Indexed::new(&kv)
                .insert(
                    &format!("tok:tok-{i}"),
                    tok_json(i).into_bytes(),
                    &[idx[0].as_str(), idx[1].as_str()],
                    None
                )
                .await
                .unwrap()
        );
    }
    // Sampler: its own connection, every 10 ms, counts other backends by (side, state, wait event).
    let side = Arc::new(AtomicUsize::new(0)); // 0 none, 1 direct, 2 persist
    let counts: Arc<Mutex<BTreeMap<(usize, String), usize>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    {
        let (pool, side, counts) = (pool.clone(), side.clone(), counts.clone());
        tokio::spawn(async move {
            loop {
                let s = side.load(Ordering::Relaxed);
                if s != 0
                    && let Ok(rows) = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
                        "SELECT state, wait_event_type, wait_event FROM pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND state IN ('active','idle in transaction')",
                    ).fetch_all(&pool).await {
                        let mut c = counts.lock().unwrap();
                        for (st, t, e) in rows {
                            let k = format!("{}/{}/{}", st.unwrap_or_default(), t.unwrap_or_else(|| "-".into()), e.unwrap_or_else(|| "-".into()));
                            *c.entry((s, k)).or_default() += 1;
                        }
                        *c.entry((s, "SAMPLES".into())).or_default() += 1;
                    }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
    }
    println!(
        "# postgres {}; {SEED} rows; {BATCH} calls per worker per side per pair; {WARM} warm-up pairs discarded; {pairs} measured pairs per cell; pool 16",
        pg.version()
    );
    println!(
        "workers\top\tpair\tdirect_ns_per_call\tpersist_ns_per_call\tratio\tcargo_rustc\tclaude\thost_cpu_pct\tcontainer_cpu"
    );
    let mut counter = 0usize;
    let mut summary = Vec::new();
    for w in [1usize, 2, 4, 8] {
        for op in [Op::Store, Op::PutOnly, Op::ControlSame, Op::ControlSlow] {
            let mut ratios = Vec::new();
            let (mut dsum, mut psum) = (0f64, 0f64);
            for pair in 0..(WARM + pairs) {
                let mut t = [0f64; 2];
                let order: [bool; 2] = if pair % 2 == 0 {
                    [true, false]
                } else {
                    [false, true]
                };
                for d in order {
                    if pair >= WARM {
                        side.store(if d { 1 } else { 2 }, Ordering::Relaxed);
                    }
                    let s = Instant::now();
                    let b = counter;
                    counter += w * BATCH;
                    let fs: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = (0..w)
                        .map(|k| {
                            Box::pin(worker(op, d, &pool, &kv, b + k * BATCH))
                                as Pin<Box<dyn Future<Output = ()> + '_>>
                        })
                        .collect();
                    join_all(fs).await;
                    t[if d { 0 } else { 1 }] = s.elapsed().as_nanos() as f64 / (w * BATCH) as f64;
                    side.store(0, Ordering::Relaxed);
                }
                if pair < WARM {
                    continue;
                }
                let ratio = t[1] / t[0];
                ratios.push(ratio);
                dsum += t[0];
                psum += t[1];
                println!(
                    "{w}\t{op:?}\t{}\t{:.0}\t{:.0}\t{ratio:.3}\t{}",
                    pair - WARM,
                    t[0],
                    t[1],
                    load()
                );
            }
            ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
            summary.push(format!(
                "W={w}\t{op:?}\tn={}\tmedian {:.3}\tp25 {:.3}\tp75 {:.3}\tmin {:.3}\tmax {:.3}\tmean direct {:.0} ns/call, mean persist {:.0} ns/call",
                ratios.len(), pct(&ratios, 0.5), pct(&ratios, 0.25), pct(&ratios, 0.75), ratios[0], ratios[ratios.len() - 1],
                dsum / pairs as f64, psum / pairs as f64
            ));
        }
        // wait-event table for this W, then reset
        let mut c = counts.lock().unwrap();
        for sd in [1usize, 2] {
            let total = c.get(&(sd, "SAMPLES".into())).copied().unwrap_or(0);
            let mut rows: Vec<_> = c
                .iter()
                .filter(|((s, k), _)| *s == sd && k != "SAMPLES")
                .map(|((_, k), v)| (k.clone(), *v))
                .collect();
            rows.sort_by_key(|a| std::cmp::Reverse(a.1));
            summary.push(format!(
                "W={w}\twaits side={} samples={total}\t{}",
                if sd == 1 { "direct" } else { "persist" },
                rows.iter()
                    .take(6)
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ; ")
            ));
        }
        c.clear();
    }
    println!("# ratio = persistant / direct per pair (above 1: the key-value layer is slower)");
    for s in summary {
        println!("{s}");
    }
}
