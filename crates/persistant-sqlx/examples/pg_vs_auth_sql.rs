// SPDX-License-Identifier: MIT OR Apache-2.0
//! ROADMAP row 30, open half: the key-value layer against auth-framework's own PostgreSQL SQL.
//! The design (what is compared, what is not, the statistic, the load fields) is
//! `bench/pg-vs-auth-sql-design.md`, written before this ran. Read it first.
//!
//! `PERSISTANT_TEST_DATABASE_URL=postgres://... PAIRS=30 [CONC=8] cargo run -q --release -p persistant-sqlx --example pg_vs_auth_sql`
//!
//! Per operation: interleaved pairs in one session (A,B then B,A alternating), 5 warm-up pairs
//! discarded, each side a batch of `BATCH` calls; the per-pair ratio persistant/direct is the datum.
//! The load (cargo/rustc and claude process counts, total CPU percent, the container's CPU percent) is
//! recorded for each pair, outside the timed batch. Two controls: `control_same` (both sides the same
//! direct statement: the ratio must centre on 1) and `control_slow` (the direct side gets `pg_sleep(0.002)`:
//! the ratio must fall well below 1).

use persistant_kv::index::Indexed;
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_relational::RelationalBackend;
use persistant_sqlx::PgBackend;
use std::process::Command;
use std::time::Instant;

const SEED: usize = 2000;
const BATCH: usize = 25;
const WARM: usize = 5;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Op {
    Token,
    Session,
    Store,
    Kv,
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
fn ses_json(i: usize) -> String {
    format!(
        r#"{{"user_id":"user-{}","session_id":"ses-{i}","pad":"{}"}}"#,
        i % 500,
        "y".repeat(220)
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

async fn run_one(
    op: Op,
    direct: bool,
    pool: &sqlx::PgPool,
    kv: &KvOnRelational<PgBackend>,
    i: usize,
) {
    let i = i % SEED;
    match (op, direct) {
        (Op::Token, true) | (Op::ControlSame, _) | (Op::ControlSlow, _) => {
            if op == Op::ControlSlow && direct {
                sqlx::query("SELECT pg_sleep(0.002)")
                    .execute(pool)
                    .await
                    .unwrap();
            }
            let r = sqlx::query("SELECT * FROM auth_tokens WHERE access_token = $1")
                .bind(tok_access(i))
                .fetch_optional(pool)
                .await
                .unwrap();
            assert!(r.is_some());
        }
        (Op::Token, false) => {
            let r = Indexed::new(kv)
                .lookup_one(&format!("tokacc:{}", tok_access(i)))
                .await
                .unwrap();
            assert!(r.is_some());
        }
        (Op::Session, true) => {
            let r = sqlx::query("SELECT data FROM sessions WHERE session_id = $1")
                .bind(format!("ses-{i}"))
                .fetch_optional(pool)
                .await
                .unwrap();
            assert!(r.is_some());
        }
        (Op::Session, false) => {
            assert!(kv.get(&format!("ses:ses-{i}")).await.unwrap().is_some());
        }
        (Op::Store, true) => direct_store_token(pool, i).await,
        (Op::Store, false) => persist_store_token(kv, i).await,
        (Op::Kv, true) => {
            let r = sqlx::query("SELECT value FROM kv_store WHERE key = $1 AND (expires_at IS NULL OR expires_at > NOW())")
                .bind(format!("key-{i}"))
                .fetch_optional(pool)
                .await
                .unwrap();
            assert!(r.is_some());
        }
        (Op::Kv, false) => {
            assert!(kv.get(&format!("kv:key-{i}")).await.unwrap().is_some());
        }
    }
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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let url =
        std::env::var("PERSISTANT_TEST_DATABASE_URL").expect("set PERSISTANT_TEST_DATABASE_URL");
    let pairs: usize = std::env::var("PAIRS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let conc = std::env::var("CONC").is_ok_and(|v| v == "8");
    let pg = PgBackend::connect(&url).await.expect("connect");
    let pool = pg.pool().clone();
    for t in ["auth_tokens", "sessions", "kv_store", "persistant_kv"] {
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {t}")))
            .execute(&pool)
            .await;
    }
    // auth-framework @ 0cec4464, PostgresStorage::migrate and SESSIONS_SCHEMA_SQL (the sessions table).
    for ddl in [
        "CREATE TABLE auth_tokens (token_id VARCHAR(255) PRIMARY KEY, user_id VARCHAR(255) NOT NULL, access_token TEXT NOT NULL UNIQUE, refresh_token TEXT, token_type VARCHAR(50), expires_at TIMESTAMPTZ NOT NULL, scopes TEXT[], issued_at TIMESTAMPTZ NOT NULL, auth_method VARCHAR(100) NOT NULL, subject VARCHAR(255), issuer VARCHAR(255), client_id VARCHAR(255), metadata JSONB, created_at TIMESTAMPTZ DEFAULT NOW(), user_profile JSONB, permissions TEXT[] NOT NULL DEFAULT '{}', roles TEXT[] NOT NULL DEFAULT '{}')",
        "CREATE INDEX idx_auth_tokens_user_id ON auth_tokens (user_id)",
        "CREATE INDEX idx_auth_tokens_access_token ON auth_tokens (access_token)",
        "CREATE INDEX idx_auth_tokens_expires_at ON auth_tokens (expires_at)",
        "CREATE TABLE sessions (session_id VARCHAR(255) PRIMARY KEY, user_id VARCHAR(255) NOT NULL, data JSONB NOT NULL, expires_at TIMESTAMPTZ)",
        "CREATE INDEX idx_sessions_user_id ON sessions (user_id)",
        "CREATE TABLE kv_store (key VARCHAR(512) PRIMARY KEY, value BYTEA NOT NULL, expires_at TIMESTAMPTZ, created_at TIMESTAMPTZ DEFAULT NOW())",
        "CREATE INDEX idx_kv_store_expires_at ON kv_store (expires_at)",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(ddl.to_string()))
            .execute(&pool)
            .await
            .unwrap();
    }
    let kv = KvOnRelational::open(pg.clone()).await.expect("kv open");
    for i in 0..SEED {
        direct_store_token(&pool, i).await;
        sqlx::query("INSERT INTO sessions (session_id, user_id, data, expires_at) VALUES ($1, $2, $3::text::jsonb, NOW() + interval '1 hour')")
            .bind(format!("ses-{i}")).bind(format!("user-{}", i % 500)).bind(ses_json(i))
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO kv_store (key, value) VALUES ($1, $2)")
            .bind(format!("key-{i}"))
            .bind(vec![7u8; 100])
            .execute(&pool)
            .await
            .unwrap();
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
        kv.put(&format!("ses:ses-{i}"), ses_json(i).into_bytes(), None)
            .await
            .unwrap();
        kv.put(&format!("kv:key-{i}"), vec![7u8; 100], None)
            .await
            .unwrap();
    }
    println!("# concurrency {}", if conc { "8 workers" } else { "1" });
    println!(
        "# postgres {}; {SEED} rows per table, {BATCH} calls per side per pair, {WARM} warm-up pairs discarded, {pairs} measured pairs per op",
        pg.version()
    );
    println!(
        "op\tpair\tdirect_ns_per_call\tpersist_ns_per_call\tratio\tcargo_rustc\tclaude\thost_cpu_pct\tcontainer_cpu"
    );
    let mut counter = 0usize;
    let mut summary = Vec::new();
    for op in [
        Op::Token,
        Op::Session,
        Op::Store,
        Op::Kv,
        Op::ControlSame,
        Op::ControlSlow,
    ] {
        let mut ratios = Vec::new();
        for pair in 0..(WARM + pairs) {
            let mut t = [0f64; 2]; // [direct, persist]
            let order: [bool; 2] = if pair % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            };
            for side in order {
                let s = Instant::now();
                if conc {
                    // 8 workers on the shared pool (16 connections), each BATCH calls: throughput per call.
                    let b = counter;
                    counter += 8 * BATCH;
                    tokio::join!(
                        worker(op, side, &pool, &kv, b),
                        worker(op, side, &pool, &kv, b + BATCH),
                        worker(op, side, &pool, &kv, b + 2 * BATCH),
                        worker(op, side, &pool, &kv, b + 3 * BATCH),
                        worker(op, side, &pool, &kv, b + 4 * BATCH),
                        worker(op, side, &pool, &kv, b + 5 * BATCH),
                        worker(op, side, &pool, &kv, b + 6 * BATCH),
                        worker(op, side, &pool, &kv, b + 7 * BATCH),
                    );
                    t[if side { 0 } else { 1 }] =
                        s.elapsed().as_nanos() as f64 / (8 * BATCH) as f64;
                } else {
                    for _ in 0..BATCH {
                        counter += 1;
                        run_one(op, side, &pool, &kv, counter).await;
                    }
                    t[if side { 0 } else { 1 }] = s.elapsed().as_nanos() as f64 / BATCH as f64;
                }
            }
            if pair < WARM {
                continue;
            }
            let ratio = t[1] / t[0];
            ratios.push(ratio);
            println!(
                "{op:?}\t{}\t{:.0}\t{:.0}\t{ratio:.3}\t{}",
                pair - WARM,
                t[0],
                t[1],
                load()
            );
        }
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        summary.push(format!(
            "{op:?}\tn={}\tmedian {:.3}\tp25 {:.3}\tp75 {:.3}\tmin {:.3}\tmax {:.3}",
            ratios.len(),
            pct(&ratios, 0.5),
            pct(&ratios, 0.25),
            pct(&ratios, 0.75),
            ratios[0],
            ratios[ratios.len() - 1]
        ));
    }
    println!("# ratio = persistant / direct per pair (above 1: the key-value layer is slower)");
    for s in summary {
        println!("{s}");
    }
}
