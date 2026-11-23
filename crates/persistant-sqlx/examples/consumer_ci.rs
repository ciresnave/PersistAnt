// SPDX-License-Identifier: MIT OR Apache-2.0
//! A consumer's CI check (ROADMAP row 18, `docs/CONSUMER-CI.md`): run PersistAnt's key-value suite
//! against **your own** production backend and fail the build when a capability you need is not there,
//! so "the backend gives what I need" is a red build, not a discovery at 3 a.m.
//!
//! ```text
//! PERSISTANT_CI_URL=postgres://user:pw@host/db \
//! PERSISTANT_NEEDS=kv.get,kv.put,kv.put_if_absent,kv.ttl \
//!   cargo run -q -p persistant-sqlx --features sqlite --example consumer_ci
//! ```
//!
//! `PERSISTANT_CI_URL` is a `postgres://` URL, a `mysql://` URL (MySQL or MariaDB; build with `--features
//! mysql`), or `sqlite:<path>` (a file the run creates and keeps) with the `sqlite` feature. `PERSISTANT_NEEDS` is a comma-separated list of key-value leaf ids. The suite
//! runs against a throwaway `persistant_kv` table set in that database, so point it at a scratch
//! database or a staging copy of the production server, never at data you need.
//!
//! Exit codes: 0 every needed leaf passed on this backend; 1 at least one did not (the verdict and its
//! reason are printed per leaf); 2 usage, cannot connect, or a needed id is not a key-value leaf this
//! suite runs (a typo is therefore a failure, never a silent pass).

use persistant_core::{Config, Row};
use persistant_kv::KvOnRelational;
use persistant_sqlx::PgBackend;
use std::process::ExitCode;

fn main() -> ExitCode {
    // The certification futures are large; a Windows main thread has a 1 MB stack. Run with room.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(run())
        })
        .expect("spawn")
        .join()
        .expect("suite thread")
}

async fn certify<R: persistant_relational::RelationalBackend + 'static>(
    db: R,
    dialect: &str,
) -> Result<Vec<Row>, String> {
    let kv = KvOnRelational::open(db)
        .await
        .map_err(|e| format!("open the key-value store: {e:?}"))?;
    let env = Config::new()
        .with("os", std::env::consts::OS)
        .with("arch", std::env::consts::ARCH);
    let config = Config::new().with("dialect", dialect);
    Ok(persistant_kv::conformance::certify(&kv, &config, &env)
        .await
        .into_iter()
        .map(|c| c.row)
        .collect())
}

async fn run() -> ExitCode {
    let (Ok(url), Ok(needs)) = (
        std::env::var("PERSISTANT_CI_URL"),
        std::env::var("PERSISTANT_NEEDS"),
    ) else {
        eprintln!(
            "set PERSISTANT_CI_URL (postgres://..., mysql://... or sqlite:<path>) and PERSISTANT_NEEDS (kv.get,kv.put,...)"
        );
        return ExitCode::from(2);
    };
    let needs: Vec<&str> = needs
        .split(',')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    if needs.is_empty() {
        eprintln!("PERSISTANT_NEEDS names no leaf");
        return ExitCode::from(2);
    }
    let rows = if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        match PgBackend::connect(&url).await {
            Ok(db) => certify(db, "postgres").await,
            Err(e) => Err(format!("connect: {e:?}")),
        }
    } else if url.starts_with("mysql://") || url.starts_with("mariadb://") {
        #[cfg(feature = "mysql")]
        {
            match persistant_sqlx::MysqlBackend::connect(&url).await {
                Ok(db) => certify(db, "mysql").await,
                Err(e) => Err(format!("connect: {e:?}")),
            }
        }
        #[cfg(not(feature = "mysql"))]
        Err("build with --features mysql for mysql:// URLs".to_string())
    } else if let Some(_path) = url.strip_prefix("sqlite:") {
        #[cfg(feature = "sqlite")]
        {
            match persistant_sqlx::SqliteBackend::open(std::path::Path::new(_path)).await {
                Ok(db) => certify(db, "sqlite").await,
                Err(e) => Err(format!("open: {e:?}")),
            }
        }
        #[cfg(not(feature = "sqlite"))]
        Err("build with --features sqlite for sqlite: URLs".to_string())
    } else {
        Err("PERSISTANT_CI_URL must start with postgres://, mysql:// or sqlite:".to_string())
    };
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let (mut unmet, mut unknown) = (0, 0);
    for need in needs {
        match rows.iter().find(|r| r.subject.to_string() == need) {
            None => {
                unknown += 1;
                println!("UNKNOWN  {need}: not a key-value leaf this suite runs");
            }
            Some(r) if r.enabled => println!("ok       {need}"),
            Some(r) => {
                unmet += 1;
                println!("MISSING  {need}: {:?}; {}", r.reason, r.note);
            }
        }
    }
    if unknown > 0 {
        ExitCode::from(2)
    } else if unmet > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
