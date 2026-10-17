// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant-migrate up|status|verify-fresh --dir DIR [--url URL]`
//!
//! The URL may also come from `PERSISTANT_DATABASE_URL` (preferred: a command line is visible to
//! other users of the machine). Schemes: `postgres://`, `postgresql://`, `mysql://`, `mariadb://`,
//! `sqlite:PATH`.
//!
//! Exit codes: 0 done; 2 usage; 3 an applied migration's text changed; 4 the database is newer than
//! the directory; 5 a migration is recorded as dirty; 6 a migration failed; 7 anything else (cannot
//! connect, cannot read the directory); 8 the command is not offered for this database.

use persistant_migrate::{State, load_dir, status};
use persistant_relational::{MigrateError, Migrates, Migration, RelError};
use persistant_sqlx::{MysqlBackend, PgBackend, SqliteBackend};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: persistant-migrate <up|status|verify-fresh> --dir DIR [--url URL]\n  \
URL defaults to $PERSISTANT_DATABASE_URL; schemes postgres://, mysql://, mariadb://, sqlite:PATH";

enum Db {
    Pg(PgBackend),
    Sqlite(SqliteBackend),
    Mysql(MysqlBackend),
}

fn code(e: &MigrateError) -> u8 {
    match e {
        MigrateError::ChecksumMismatch { .. } => 3,
        MigrateError::NewerThanProgram { .. } => 4,
        MigrateError::Dirty { .. } => 5,
        MigrateError::Failed { .. } => 6,
        _ => 7,
    }
}

fn fail(msg: impl std::fmt::Display, code: u8) -> ExitCode {
    eprintln!("persistant-migrate: {msg}");
    ExitCode::from(code)
}

async fn connect(url: &str) -> Result<Db, String> {
    let err = |e: RelError| format!("cannot connect: {e}");
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        PgBackend::connect(url).await.map(Db::Pg).map_err(err)
    } else if let Some(rest) = url.strip_prefix("mariadb://") {
        MysqlBackend::connect(&format!("mysql://{rest}"))
            .await
            .map(Db::Mysql)
            .map_err(err)
    } else if url.starts_with("mysql://") {
        MysqlBackend::connect(url).await.map(Db::Mysql).map_err(err)
    } else if let Some(path) = url.strip_prefix("sqlite:") {
        let path = path.strip_prefix("//").unwrap_or(path);
        SqliteBackend::open(path).await.map(Db::Sqlite).map_err(err)
    } else {
        Err("the URL scheme is not one of postgres, mysql, mariadb, sqlite".to_string())
    }
}

async fn run(cmd: &str, db: &Db, migrations: &[Migration]) -> ExitCode {
    match cmd {
        "up" => {
            let r = match db {
                Db::Pg(b) => b.migrate(migrations).await,
                Db::Sqlite(b) => b.migrate(migrations).await,
                Db::Mysql(b) => b.migrate(migrations).await,
            };
            match r {
                Ok(r) => {
                    println!(
                        "applied {:?}; already applied {:?}",
                        r.applied, r.already_applied
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e, code(&e)),
            }
        }
        "status" => {
            let ledger = match db {
                Db::Pg(b) => b.applied_checksums().await,
                Db::Sqlite(b) => b.applied_checksums().await,
                Db::Mysql(b) => b.applied_checksums().await,
            };
            let ledger = match ledger {
                Ok(l) => l,
                Err(e) => return fail(&e, code(&e)),
            };
            let lines = status(migrations, &ledger);
            for l in &lines {
                let s = match l.state {
                    State::Applied => "applied",
                    State::Pending => "pending",
                    State::Changed => "CHANGED",
                    State::Unknown => "UNKNOWN",
                };
                println!("{:>14}  {s:<8} {}", l.version, l.description);
            }
            if lines.iter().any(|l| l.state == State::Changed) {
                ExitCode::from(3)
            } else if lines.iter().any(|l| l.state == State::Unknown) {
                ExitCode::from(4)
            } else {
                ExitCode::SUCCESS
            }
        }
        "verify-fresh" => {
            let r = match db {
                Db::Pg(b) => b.verify_fresh(migrations).await,
                Db::Sqlite(_) => {
                    // A throwaway database file, deleted with its directory.
                    let dir = match tempfile::tempdir() {
                        Ok(d) => d,
                        Err(e) => return fail(e, 7),
                    };
                    match SqliteBackend::open(dir.path().join("fresh.db")).await {
                        Ok(b) => b.migrate(migrations).await,
                        Err(e) => return fail(e, 7),
                    }
                }
                Db::Mysql(_) => {
                    return fail("verify-fresh is not offered for MySQL or MariaDB", 8);
                }
            };
            match r {
                Ok(r) => {
                    println!(
                        "installs on an empty database: {} migrations",
                        r.applied.len()
                    );
                    ExitCode::SUCCESS
                }
                Err(e) => fail(&e, code(&e)),
            }
        }
        _ => fail(USAGE, 2),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        return fail(USAGE, 2);
    };
    let mut dir = None::<PathBuf>;
    let mut url = std::env::var("PERSISTANT_DATABASE_URL").ok();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "--url" => url = args.next(),
            _ => return fail(USAGE, 2),
        }
    }
    let (Some(dir), Some(url)) = (dir, url) else {
        return fail(USAGE, 2);
    };
    let migrations = match load_dir(&dir).await {
        Ok(m) => m,
        Err(e) => return fail(e, 7),
    };
    let db = match connect(&url).await {
        Ok(d) => d,
        Err(e) => return fail(e, 7),
    };
    run(&cmd, &db, &migrations).await
}
