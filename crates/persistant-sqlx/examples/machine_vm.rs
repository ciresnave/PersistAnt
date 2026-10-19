// SPDX-License-Identifier: MIT OR Apache-2.0
//! The guest side of the `Machine` durability test (`scripts/machine-durability/`): run **inside** a
//! Linux VM that the host then powers off with `Stop-VM -TurnOff`.
//!
//! ```text
//! machine_vm write  <backend> <spec>            put numbered keys, print "acked N" after each put returns
//! machine_vm check  <backend> <spec> <count>    keys 0..count must all be present with their value
//! machine_vm enospc <backend> <spec>            write until a put fails; it must fail, never ack silently
//! ```
//!
//! `<backend>` is `sqlite` (`synchronous=FULL`), `sqlite-off` (`synchronous=OFF`: **the known-bad
//! control**, which acknowledges before the data is on stable storage), `postgres` or `mysql`; `<spec>`
//! is a file path or a connection URL. The keys and values are those of `kv.durability.process`
//! (`persistant_kv::durability`), so a writer here and a checker there agree.
//!
//! Exit codes: 0 done; 1 an acknowledged key is missing or holds another value; 2 usage or cannot
//! open; 5 `check`/`enospc` could not connect (retried for two minutes); 6 `check` could not read (errors, not
//! losses); 3 `enospc` saw a put fail (the expected, loud outcome); 4 `enospc` wrote its whole budget
//! without a failure (the disk was not full, so the run proves nothing).

use persistant_kv::durability::{Mode, key, value};
use persistant_kv::{KvBackend, KvOnRelational};
use persistant_sqlx::{MysqlBackend, PgBackend, SqliteBackend};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::io::Write;
use std::process::ExitCode;

/// How many keys `write` and `enospc` try before giving up (the host cuts power long before).
const BUDGET: u32 = 1_000_000;

/// How long `enospc` waits for one put before calling it failed.
const STALL: std::time::Duration = std::time::Duration::from_secs(60);

async fn sqlite(path: &str, sync: SqliteSynchronous) -> KvOnRelational<SqliteBackend> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(sync)
        .busy_timeout(std::time::Duration::from_secs(30));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await
        .expect("open sqlite");
    let db = SqliteBackend::from_pool(pool)
        .await
        .expect("sqlite backend");
    KvOnRelational::open(db).await.expect("open kv")
}

async fn write<K: KvBackend>(kv: &K) -> ExitCode {
    for n in 0..BUDGET {
        if let Err(e) = kv.put(&key(Mode::Honest, n), value(n), None).await {
            eprintln!("put {n} failed: {e}");
            return ExitCode::from(3);
        }
        // The host reads this over the serial port and cuts power right after seeing it.
        println!("acked {n}");
        let _ = std::io::stdout().flush();
    }
    ExitCode::SUCCESS
}

async fn check<K: KvBackend>(kv: &K, count: u32) -> ExitCode {
    // A key is LOST only when the store answers and the answer is "absent" or a different value. A read
    // that errors or does not answer is UNREADABLE: it proves nothing either way and is reported as such
    // (exit 6), never as a loss. The server may still be recovering from a full disk.
    let mut lost = Vec::new();
    let mut unreadable: Vec<(u32, String)> = Vec::new();
    let mut held = 0_u32;
    for n in 0..count {
        match tokio::time::timeout(STALL, kv.get(&key(Mode::Honest, n))).await {
            Ok(Ok(Some(v))) if v == value(n) => held += 1,
            Ok(Ok(_)) => lost.push(n),
            Ok(Err(e)) => unreadable.push((n, e.to_string())),
            Err(_) => unreadable.push((n, format!("no answer in {STALL:?}"))),
        }
        if unreadable.len() >= 5 {
            break;
        }
    }
    if !lost.is_empty() {
        println!(
            "lost {} of {count} (held {held}, unreadable {}); first missing keys {:?}",
            lost.len(),
            unreadable.len(),
            &lost[..lost.len().min(8)]
        );
        ExitCode::from(1)
    } else if !unreadable.is_empty() {
        println!(
            "unreadable: {} errors at keys {:?}, stopped there (held {held} of {count}, none proven lost); first error: {}",
            unreadable.len(),
            unreadable.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            unreadable[0].1
        );
        ExitCode::from(6)
    } else {
        println!("held {count}");
        ExitCode::SUCCESS
    }
}

async fn enospc<K: KvBackend>(kv: &K) -> ExitCode {
    let mut acked = 0_u32;
    for n in 0..BUDGET {
        // A server that cannot write may wait for space instead of returning an error (InnoDB does).
        // A put that never returns was never acknowledged, so it is reported as a failure, by name.
        let put = tokio::time::timeout(STALL, kv.put(&key(Mode::Honest, n), value(n), None)).await;
        match put {
            Ok(Ok(())) => {
                acked = n + 1;
                // The host reads these to tell a slow fill from a hang, and checks up to the last one.
                if acked.is_multiple_of(500) {
                    println!("progress {acked}");
                    let _ = std::io::stdout().flush();
                }
            }
            Ok(Err(e)) => {
                println!("failed {n} after acked {acked}: {e}");
                return ExitCode::from(3);
            }
            Err(_) => {
                println!("failed {n} after acked {acked}: stalled, no answer in {STALL:?}");
                return ExitCode::from(3);
            }
        }
    }
    println!("never failed; acked {acked}");
    ExitCode::from(4)
}

/// Connect and open the kv store, retrying for two minutes: a server that has just recovered from a
/// full disk, or a restart, refuses connections for a while, which says nothing about the data.
async fn connect_retry<T, Fut: std::future::Future<Output = Result<T, String>>>(
    mut open: impl FnMut() -> Fut,
) -> Result<T, String> {
    let mut last = String::new();
    for _ in 0..60 {
        match open().await {
            Ok(t) => return Ok(t),
            Err(e) => last = e,
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Err(last)
}

async fn run(cmd: &str, backend: &str, spec: &str, count: u32) -> ExitCode {
    macro_rules! with {
        ($kv:expr) => {{
            let kv = $kv;
            match cmd {
                "write" => write(&kv).await,
                "check" => check(&kv, count).await,
                _ => enospc(&kv).await,
            }
        }};
    }
    macro_rules! served {
        ($open:expr) => {
            match connect_retry(|| $open).await {
                Ok(kv) => with!(kv),
                Err(e) => {
                    println!("cannot connect to {backend} after 2 minutes of retries: {e}");
                    ExitCode::from(5)
                }
            }
        };
    }
    match backend {
        "sqlite" => with!(sqlite(spec, SqliteSynchronous::Full).await),
        "sqlite-off" => with!(sqlite(spec, SqliteSynchronous::Off).await),
        "postgres" => served!(async {
            let db = PgBackend::connect(spec)
                .await
                .map_err(|e| format!("{e:?}"))?;
            KvOnRelational::open(db).await.map_err(|e| format!("{e:?}"))
        }),
        "mysql" => served!(async {
            let db = MysqlBackend::connect(spec)
                .await
                .map_err(|e| format!("{e:?}"))?;
            KvOnRelational::open(db).await.map_err(|e| format!("{e:?}"))
        }),
        other => {
            eprintln!("unknown backend {other}");
            ExitCode::from(2)
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (Some(cmd), Some(backend), Some(spec)) = (a.first(), a.get(1), a.get(2)) else {
        eprintln!("usage: machine_vm <write|check|enospc> <backend> <spec> [count]");
        return ExitCode::from(2);
    };
    let count = a.get(3).and_then(|c| c.parse().ok()).unwrap_or(0);
    if !matches!(cmd.as_str(), "write" | "enospc") && !(cmd == "check" && count > 0) {
        eprintln!("usage: machine_vm <write|check|enospc> <backend> <spec> [count]");
        return ExitCode::from(2);
    }
    run(cmd, backend, spec, count).await
}
