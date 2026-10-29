// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant-migrate up|status|verify-fresh --dir DIR [--url URL] [--allow-remote]`
//!
//! `persistant-migrate snapshot [--scratch --dir DIR] [--url URL] [--out FILE] [--check] [--allow-remote]`
//! writes the schema snapshot (`persistant-schema.json` by default; structure only, no data): without
//! `--scratch` it reads the catalog of the target itself; with `--scratch` it applies the migrations
//! of `--dir` to a scratch schema or database (or a temporary SQLite file), reads that, and drops it,
//! so the target is never touched. The file records the migrations it was derived from. With
//! `--check` nothing is written: the snapshot is derived again and the command fails (exit 8) if it
//! differs from the file, saying what changed ("stale: migration 37 is not in the snapshot").
//!
//! `snapshot --from-files --dir DIR --dialect postgres|mysql|mariadb|sqlite` needs no database and no
//! URL: UniSQL reads the migrations as DDL (`CREATE TABLE`, `ALTER TABLE .. ADD|DROP|ALTER COLUMN`,
//! `DROP TABLE`). The snapshot says `source = ddl`; types keep the migration's spelling and statements
//! UniSQL cannot read are listed on stderr. `--check` re-derives it the same way.
//!
//! The URL may also come from `PERSISTANT_DATABASE_URL` (preferred: a command line is visible to
//! other users of the machine). Schemes: `postgres://`, `postgresql://`, `mysql://`, `mariadb://`,
//! `sqlite:PATH`.
//!
//! Exit codes: 0 done; 2 usage; 3 an applied migration's text changed; 4 the database is newer than
//! the directory; 5 a migration is recorded as dirty; 6 a migration failed; 7 anything else (cannot
//! connect, cannot read the directory); 8 `snapshot --check` found the file stale or different; 9 the target is not on this machine and `--allow-remote` was
//! not given.
//!
//! Every run prints its target (host, port, database; never the password) before it connects, and a
//! target that is not loopback, a unix socket or an SQLite file is refused without `--allow-remote`:
//! a wrong URL in an environment variable must not silently migrate someone else's database.

use persistant_migrate::{State, describe_target, load_dir, snapshot_from_files, status};
use persistant_relational::catalog::{self, Snapshot};
use persistant_relational::{MigrateError, Migrates, Migration, RelError};
use persistant_sqlx::{MysqlBackend, PgBackend, SqliteBackend};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: persistant-migrate <up|status|verify-fresh> --dir DIR [--url URL] [--allow-remote]\n       \
persistant-migrate snapshot [--scratch --dir DIR] [--url URL] [--out FILE] [--check] [--allow-remote]\n       \
persistant-migrate snapshot --from-files --dir DIR --dialect D [--out FILE] [--check]\n  \
URL defaults to $PERSISTANT_DATABASE_URL; schemes postgres://, mysql://, mariadb://, sqlite:PATH";

/// Where `snapshot` writes and `snapshot --check` reads, unless `--out` says otherwise.
const DEFAULT_SNAPSHOT: &str = "persistant-schema.json";

struct Snap {
    scratch: bool,
    from_files: bool,
    dialect: Option<String>,
    check: bool,
    out: PathBuf,
}

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

/// The catalog `snapshot` is about: the target itself, or a scratch copy built from `migrations`.
async fn derive(db: &Db, migrations: &[Migration], scratch: bool) -> Result<Snapshot, String> {
    let stamps_of = |l: Result<Vec<(i64, Vec<u8>)>, MigrateError>| {
        l.map(|l| catalog::migration_meta(&l))
            .map_err(|e| e.to_string())
    };
    if scratch {
        return match db {
            Db::Pg(b) => b.fresh_catalog(migrations).await.map_err(|e| e.to_string()),
            Db::Mysql(b) => b.fresh_catalog(migrations).await.map_err(|e| e.to_string()),
            Db::Sqlite(_) => {
                // A throwaway database file, deleted with its directory.
                let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
                let b = SqliteBackend::open(dir.path().join("fresh.db"))
                    .await
                    .map_err(|e| e.to_string())?;
                b.migrate(migrations).await.map_err(|e| e.to_string())?;
                let stamps = stamps_of(b.applied_checksums().await)?;
                catalog::read_catalog(&b, stamps)
                    .await
                    .map_err(|e| e.to_string())
            }
        };
    }
    match db {
        Db::Pg(b) => {
            let stamps = stamps_of(b.applied_checksums().await)?;
            catalog::read_catalog(b, stamps).await
        }
        Db::Mysql(b) => {
            let stamps = stamps_of(b.applied_checksums().await)?;
            catalog::read_catalog(b, stamps).await
        }
        Db::Sqlite(b) => {
            let stamps = stamps_of(b.applied_checksums().await)?;
            catalog::read_catalog(b, stamps).await
        }
    }
    .map_err(|e| e.to_string())
}

async fn snapshot(db: Option<&Db>, migrations: &[Migration], snap: &Snap) -> ExitCode {
    let now = match (db, &snap.dialect) {
        (Some(db), _) => match derive(db, migrations, snap.scratch).await {
            Ok(c) => c,
            Err(e) => return fail(e, 7),
        },
        (None, Some(dialect)) => match snapshot_from_files(migrations, dialect) {
            Ok((c, skipped)) => {
                for s in &skipped {
                    eprintln!("persistant-migrate: not read as DDL: {s}");
                }
                c
            }
            Err(e) => return fail(e, 2),
        },
        (None, None) => return fail(USAGE, 2),
    };
    if snap.check {
        let text = match std::fs::read_to_string(&snap.out) {
            Ok(t) => t,
            Err(e) => {
                return fail(
                    format!("no snapshot to check at {}: {e}", snap.out.display()),
                    8,
                );
            }
        };
        let committed = match catalog::parse(&text) {
            Ok(c) => c,
            Err(e) => return fail(format!("{}: {e}", snap.out.display()), 8),
        };
        let diffs = catalog::differences(&committed, &now);
        if diffs.is_empty() {
            println!(
                "{} is current: {} tables, {} migrations",
                snap.out.display(),
                now.schema.tables().count(),
                now.meta
                    .keys()
                    .filter(|k| k.starts_with("migration."))
                    .count()
            );
            return ExitCode::SUCCESS;
        }
        for d in &diffs {
            eprintln!("  {d}");
        }
        return fail(
            format!(
                "{} is out of date ({} differences); run `persistant-migrate snapshot` and commit it",
                snap.out.display(),
                diffs.len()
            ),
            8,
        );
    }
    if let Some(parent) = snap.out.parent().filter(|p| !p.as_os_str().is_empty())
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return fail(format!("cannot create {}: {e}", parent.display()), 7);
    }
    if let Err(e) = std::fs::write(&snap.out, now.to_json()) {
        return fail(format!("cannot write {}: {e}", snap.out.display()), 7);
    }
    println!(
        "wrote {}: {} tables, {} migrations",
        snap.out.display(),
        now.schema.tables().count(),
        now.meta
            .keys()
            .filter(|k| k.starts_with("migration."))
            .count()
    );
    ExitCode::SUCCESS
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
                Db::Mysql(b) => b.verify_fresh(migrations).await,
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
    // The basics come first, before any other parsing: `--help` and `--version` print on stdout and
    // exit 0 wherever they appear; a subcommand this tool does not have exits 2 before a URL is read
    // or a connection tried.
    let all: Vec<String> = std::env::args().skip(1).collect();
    if all.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    if all.iter().any(|a| a == "--version" || a == "-V") {
        println!("persistant-migrate {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut args = all.into_iter();
    let Some(cmd) = args.next() else {
        return fail(USAGE, 2);
    };
    if !matches!(cmd.as_str(), "up" | "status" | "verify-fresh" | "snapshot") {
        return fail(USAGE, 2);
    }
    let mut dir = None::<PathBuf>;
    let mut url = std::env::var("PERSISTANT_DATABASE_URL").ok();
    let mut allow_remote = false;
    let mut url_given = false;
    let mut snap = Snap {
        scratch: false,
        from_files: false,
        dialect: None,
        check: false,
        out: PathBuf::from(DEFAULT_SNAPSHOT),
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dir" => dir = args.next().map(PathBuf::from),
            "--url" => {
                url = args.next();
                url_given = true;
            }
            "--allow-remote" => allow_remote = true,
            "--scratch" if cmd == "snapshot" => snap.scratch = true,
            "--check" if cmd == "snapshot" => snap.check = true,
            "--from-files" if cmd == "snapshot" => snap.from_files = true,
            "--dialect" if cmd == "snapshot" => match args.next() {
                Some(d) => snap.dialect = Some(d),
                None => return fail(USAGE, 2),
            },
            "--out" if cmd == "snapshot" => match args.next() {
                Some(p) => snap.out = PathBuf::from(p),
                None => return fail(USAGE, 2),
            },
            _ => return fail(USAGE, 2),
        }
    }
    // `snapshot` of the target itself reads no directory; every other use needs one.
    let needs_dir = cmd != "snapshot" || snap.scratch || snap.from_files;
    if snap.from_files {
        // No database: the files alone, so no URL, no target line and no remote guard.
        if cmd != "snapshot" || snap.scratch || url_given || snap.dialect.is_none() {
            return fail(USAGE, 2);
        }
        let Some(d) = &dir else {
            return fail(USAGE, 2);
        };
        let migrations = match load_dir(d).await {
            Ok(m) => m,
            Err(e) => return fail(e, 7),
        };
        return snapshot(None, &migrations, &snap).await;
    }
    let Some(url) = url else {
        return fail(USAGE, 2);
    };
    if needs_dir && dir.is_none() {
        return fail(USAGE, 2);
    }
    let target = describe_target(&url);
    eprintln!("persistant-migrate: {cmd} on {}", target.display);
    if !target.local && !allow_remote {
        return fail(
            "that target is not on this machine; pass --allow-remote if it is the one you mean",
            9,
        );
    }
    let migrations = match &dir {
        Some(d) => match load_dir(d).await {
            Ok(m) => m,
            Err(e) => return fail(e, 7),
        },
        None => Vec::new(),
    };
    let db = match connect(&url).await {
        Ok(d) => d,
        Err(e) => return fail(e, 7),
    };
    if cmd == "snapshot" {
        return snapshot(Some(&db), &migrations, &snap).await;
    }
    run(&cmd, &db, &migrations).await
}
