// SPDX-License-Identifier: MIT OR Apache-2.0
//! The guest side of the `blob.durable` power-loss test (`scripts/machine-durability/`): run **inside**
//! a Linux VM that the host then powers off with `Stop-VM -TurnOff`. The blob counterpart of
//! `machine_vm` (the kv test); same protocol, same exit codes.
//!
//! ```text
//! machine_vm_blob write <backend> <dir>           put numbered files, print "acked N" after each put returns
//! machine_vm_blob check <backend> <dir> <count>   files 0..count must all be present with their value
//! machine_vm_blob row backend=<b> cycles=N held=N errors=N acked=N synced_cycles=N synced_held=N
//!                     unsynced_cycles=N unsynced_lost=N
//!                                                 print the `blob.durable` matrix row for what the host measured
//! ```
//!
//! `<backend>` is
//!
//! - `opendal-fs`: `OpendalBlob::fs` (no scratch directory), `write`: **the target**.
//! - `opendal-fs-atomic`: `OpendalBlob::fs` with `atomic_write_dir` (a directory next to `<dir>`),
//!   `replace_atomic`: the target's other write path.
//! - `std-synced`: **the positive control**: write a temp file, `fsync` it, rename it, `fsync` the
//!   parent directory. It must HOLD every acknowledged file, or the harness cannot show a pass.
//! - `std-unsynced`: **the known-bad control**: a plain `std::fs::write`, no sync of any kind, with the
//!   guest's dirty-page timers raised. It must LOSE files, or the harness cannot show a loss.
//!
//! Exit codes: 0 done; 1 an acknowledged file is missing or holds another value; 2 usage or cannot
//! open; 3 a put failed; 6 `check` could not read (errors, not losses).

use persistant_blob::BlobBackend;
use persistant_kv::durability::value;
use persistant_opendal::OpendalBlob;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// How many files `write` tries before giving up (the host cuts power long before).
const BUDGET: u32 = 1_000_000;

fn name(n: u32) -> String {
    format!("blob-{n:06}")
}

/// The store the commands run on.
enum Store {
    Opendal(OpendalBlob, bool),
    Std(PathBuf, bool),
}

impl Store {
    fn open(backend: &str, dir: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        match backend {
            "opendal-fs" => OpendalBlob::fs(dir, None)
                .map(|b| Store::Opendal(b, false))
                .map_err(|e| format!("{e:?}")),
            "opendal-fs-atomic" => {
                let scratch = dir.with_extension("scratch");
                std::fs::create_dir_all(&scratch)
                    .map_err(|e| format!("create {}: {e}", scratch.display()))?;
                OpendalBlob::fs(dir, Some(&scratch))
                    .map(|b| Store::Opendal(b, true))
                    .map_err(|e| format!("{e:?}"))
            }
            "std-synced" => Ok(Store::Std(dir.to_path_buf(), true)),
            "std-unsynced" => Ok(Store::Std(dir.to_path_buf(), false)),
            other => Err(format!("unknown backend {other}")),
        }
    }

    async fn put(&self, n: u32) -> Result<(), String> {
        match self {
            Store::Opendal(b, atomic) => {
                let r = if *atomic {
                    b.replace_atomic(&name(n), value(n)).await
                } else {
                    b.write(&name(n), value(n)).await
                };
                r.map_err(|e| format!("{e:?}"))
            }
            Store::Std(dir, synced) => {
                let path = dir.join(name(n));
                if !synced {
                    return std::fs::write(&path, value(n)).map_err(|e| e.to_string());
                }
                let tmp = dir.join(format!("{}.tmp", name(n)));
                let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
                f.write_all(&value(n)).map_err(|e| e.to_string())?;
                f.sync_all().map_err(|e| e.to_string())?;
                drop(f);
                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
                // The parent directory entry is what the rename changed.
                std::fs::File::open(dir)
                    .and_then(|d| d.sync_all())
                    .map_err(|e| e.to_string())
            }
        }
    }

    async fn get(&self, n: u32) -> Result<Option<Vec<u8>>, String> {
        match self {
            Store::Opendal(b, _) => b.read(&name(n)).await.map_err(|e| format!("{e:?}")),
            Store::Std(dir, _) => match std::fs::read(dir.join(name(n))) {
                Ok(v) => Ok(Some(v)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.to_string()),
            },
        }
    }
}

async fn write(store: &Store) -> ExitCode {
    for n in 0..BUDGET {
        if let Err(e) = store.put(n).await {
            eprintln!("put {n} failed: {e}");
            return ExitCode::from(3);
        }
        // The host reads this over the serial port and cuts power right after seeing it.
        println!("acked {n}");
        let _ = std::io::stdout().flush();
    }
    ExitCode::SUCCESS
}

async fn check(store: &Store, count: u32) -> ExitCode {
    // LOST only when the store answers and the answer is "absent" or another value (an empty or torn
    // file included); a read that errors is UNREADABLE and proves nothing either way (exit 6).
    let mut lost = Vec::new();
    let mut unreadable: Vec<(u32, String)> = Vec::new();
    let mut held = 0_u32;
    for n in 0..count {
        match store.get(n).await {
            Ok(Some(v)) if v == value(n) => held += 1,
            Ok(_) => lost.push(n),
            Err(e) => unreadable.push((n, e)),
        }
        if unreadable.len() >= 5 {
            break;
        }
    }
    if !lost.is_empty() {
        println!(
            "lost {} of {count} (held {held}, unreadable {}); first missing files {:?}",
            lost.len(),
            unreadable.len(),
            &lost[..lost.len().min(8)]
        );
        ExitCode::from(1)
    } else if !unreadable.is_empty() {
        println!(
            "unreadable: {} errors at files {:?} (held {held} of {count}, none proven lost); first error: {}",
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

/// `row key=value ...`: the host measured; this prints the matrix row (or nothing, if nothing was
/// measured) through the same function the matrix test checks. Exit 2 on a bad argument.
fn row(args: &[String]) -> ExitCode {
    let get = |k: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("{k}=")))
            .map(str::to_string)
    };
    let num = |k: &str| get(k).and_then(|v| v.parse::<u32>().ok());
    let (Some(backend), Some(cycles), Some(held), Some(errors), Some(acked)) = (
        get("backend"),
        num("cycles"),
        num("held"),
        num("errors"),
        num("acked"),
    ) else {
        eprintln!("row: backend, cycles, held, errors and acked are required");
        return ExitCode::from(2);
    };
    let (Some(synced_cycles), Some(synced_held), Some(unsynced_cycles), Some(unsynced_lost)) = (
        num("synced_cycles"),
        num("synced_held"),
        num("unsynced_cycles"),
        num("unsynced_lost"),
    ) else {
        eprintln!("row: the control counts are required");
        return ExitCode::from(2);
    };
    let m = persistant_opendal::machine::Measured {
        backend,
        cycles,
        held,
        errors,
        acked,
        synced_cycles,
        synced_held,
        unsynced_cycles,
        unsynced_lost,
    };
    if let Some(r) = persistant_opendal::machine::blob_durable_row(&m, opendal::raw::VERSION) {
        // Rows only: the host writes the file's header and joins the backends.
        print!("{}", persistant_core::Matrix::from_rows(vec![r]).to_tsv());
    }
    ExitCode::SUCCESS
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.first().map(String::as_str) == Some("row") {
        return row(&a[1..]);
    }
    let usage = "usage: machine_vm_blob <write|check> <backend> <dir> [count]";
    let (Some(cmd), Some(backend), Some(dir)) = (a.first(), a.get(1), a.get(2)) else {
        eprintln!("{usage}");
        return ExitCode::from(2);
    };
    let count = a.get(3).and_then(|c| c.parse().ok()).unwrap_or(0);
    if !(cmd == "write" || (cmd == "check" && count > 0)) {
        eprintln!("{usage}");
        return ExitCode::from(2);
    }
    let store = match Store::open(backend, Path::new(dir)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open {backend} at {dir}: {e}");
            return ExitCode::from(2);
        }
    };
    if cmd == "write" {
        write(&store).await
    } else {
        check(&store, count).await
    }
}
