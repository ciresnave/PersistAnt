// SPDX-License-Identifier: MIT OR Apache-2.0
//! The crash half of `blob.replace_atomic`: "a crash leaves the whole old value or the whole new
//! one". Decided by killing a process in the middle of a replace.
//!
//! A host binary (an example, or a test binary) re-runs **itself** as the child that replaces a
//! 16 MiB value in a loop; the parent waits until the child has completed one replace, waits a
//! short, varied time, kills the child hard (no destructors, no flush) and reads the key. The key
//! must hold the whole `A...` or the whole `B...` value. Three child modes:
//!
//! - [`Mode::Opendal`]: the target, `OpendalBlob::fs` with `atomic_write_dir` set.
//! - [`Mode::Rename`]: the **positive control**, a plain std write-to-temp-then-rename. The test
//!   can pass.
//! - [`Mode::InPlace`]: the **known-bad control**, a slow in-place truncate-and-write. The test must
//!   find it torn, or it proves nothing.
//!
//! The host calls [`maybe_run_child`] first thing; it returns at once unless it was started as a
//! child. What this does **not** test: power loss (data not yet on the disk), which is
//! `blob.durable` and needs a VM; a process kill leaves the operating system's page cache intact.

use crate::OpendalBlob;
use persistant_blob::{BlobBackend, BlobError};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The environment variable that makes a host binary behave as the child.
pub const ENV: &str = "PERSISTANT_CRASH_CHILD";

const LEN: usize = 16 * 1024 * 1024;
const KEY: &str = "crash/value";
const CHUNK: usize = 256 * 1024;
const ROUNDS: usize = 12;
const DELAYS_MS: [u64; 6] = [3, 7, 12, 20, 35, 50];

/// Which replacement strategy the child uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `OpendalBlob::fs` with `atomic_write_dir`, through `replace_atomic`.
    Opendal,
    /// Write a temporary file in the scratch directory, then rename it over the key.
    Rename,
    /// Truncate the key and write it in chunks, slowly.
    InPlace,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::Opendal => "opendal",
            Mode::Rename => "rename",
            Mode::InPlace => "inplace",
        }
    }
}

/// If this process was started as the crash child, run it (it never returns). Otherwise return.
pub fn maybe_run_child() {
    let Ok(spec) = std::env::var(ENV) else {
        return;
    };
    let parts: Vec<&str> = spec.split('|').collect();
    assert_eq!(parts.len(), 3, "bad {ENV}: {spec}");
    let (mode, root, scratch) = (parts[0], Path::new(parts[1]), Path::new(parts[2]));
    match mode {
        "opendal" => run_opendal_child(root, scratch),
        "rename" => run_std_child(root, scratch, true),
        "inplace" => run_std_child(root, scratch, false),
        other => panic!("unknown crash mode {other}"),
    }
}

fn announce_ready() {
    println!("ready");
    let _ = std::io::stdout().flush();
}

fn value(i: usize) -> u8 {
    if i.is_multiple_of(2) { b'A' } else { b'B' }
}

fn run_opendal_child(root: &Path, scratch: &Path) -> ! {
    let blob = OpendalBlob::fs(root, Some(scratch)).expect("open fs");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(async {
        let mut i = 0usize;
        loop {
            blob.replace_atomic(KEY, vec![value(i); LEN])
                .await
                .expect("replace_atomic");
            if i == 0 {
                announce_ready();
            }
            i += 1;
        }
    })
}

fn write_chunked(path: &Path, byte: u8) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    let chunk = vec![byte; CHUNK];
    for _ in 0..(LEN / CHUNK) {
        f.write_all(&chunk)?;
        // Slow on purpose: a long window in which a kill finds the write unfinished.
        std::thread::sleep(Duration::from_millis(2));
    }
    f.flush()
}

fn run_std_child(root: &Path, scratch: &Path, rename: bool) -> ! {
    let target = root.join(KEY);
    std::fs::create_dir_all(target.parent().unwrap()).expect("mkdir");
    let temp = scratch.join("child-temp");
    let mut i = 0usize;
    loop {
        if rename {
            write_chunked(&temp, value(i)).expect("write temp");
            std::fs::rename(&temp, &target).expect("rename");
        } else {
            write_chunked(&target, value(i)).expect("write in place");
        }
        if i == 0 {
            announce_ready();
        }
        i += 1;
    }
}

/// What a run of kills found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Kills performed.
    pub rounds: usize,
    /// Kills after which the key was not a whole `A...` or `B...` value.
    pub torn: usize,
    /// What the first torn state looked like.
    pub first_torn: Option<String>,
}

fn whole(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() != LEN {
        return Err(format!("{} bytes, want {LEN}", bytes.len()));
    }
    let first = bytes[0];
    if first != b'A' && first != b'B' {
        return Err(format!("first byte {first}"));
    }
    match bytes.iter().position(|b| *b != first) {
        None => Ok(()),
        Some(at) => Err(format!("a mix: byte {at} differs from byte 0")),
    }
}

/// Kill a child in `mode` [`ROUNDS`] times and read the key each time.
///
/// `child_args` is how this host binary is told to run [`maybe_run_child`]: empty for an example
/// whose `main` calls it first; the libtest filter (`["child_entry", "--exact", "--nocapture",
/// "--test-threads=1"]`) for a test binary.
pub fn run(
    mode: Mode,
    root: &Path,
    scratch: &Path,
    child_args: &[&str],
) -> std::io::Result<Report> {
    let spec = format!(
        "{}|{}|{}",
        mode.word(),
        root.to_string_lossy(),
        scratch.to_string_lossy()
    );
    let target = root.join(KEY);
    let mut report = Report {
        rounds: 0,
        torn: 0,
        first_torn: None,
    };
    for round in 0..ROUNDS {
        let mut child = Command::new(std::env::current_exe()?)
            .args(child_args)
            .env(ENV, &spec)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut out = BufReader::new(child.stdout.take().expect("piped"));
        let mut line = String::new();
        loop {
            line.clear();
            if out.read_line(&mut line)? == 0 {
                let _ = child.wait();
                return Err(std::io::Error::other(format!(
                    "the {} child exited before reporting ready",
                    mode.word()
                )));
            }
            // libtest prefixes the line with "test <name> ... " under --nocapture.
            if line.trim().ends_with("ready") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(DELAYS_MS[round % DELAYS_MS.len()]));
        child.kill()?; // hard stop: no destructors, no flush
        child.wait()?;
        report.rounds += 1;
        let verdict = match std::fs::read(&target) {
            Ok(bytes) => whole(&bytes),
            Err(e) => Err(format!("the key could not be read: {e}")),
        };
        if let Err(why) = verdict {
            report.torn += 1;
            report.first_torn.get_or_insert(why);
        }
    }
    Ok(report)
}

/// Certify `blob.replace_atomic` (the whole contract, crash included) for `OpendalBlob::fs`, with
/// and without `atomic_write_dir`, and return the two rows.
///
/// With the scratch directory the target is killed and judged against both controls. Without it the
/// driver itself refuses `replace_atomic` (it writes in place), so the row is `missing` and no
/// process is killed.
pub fn certify_replace(
    work: &Path,
    child_args: &[&str],
    env: &Config,
) -> std::io::Result<Vec<Row>> {
    let id = CapabilityId::new("blob.replace_atomic").expect("valid id");
    let version = opendal::raw::VERSION.to_string();
    let mut rows = Vec::new();

    // atomic_write_dir unset: ask the driver, do not kill anything.
    {
        let root = work.join("unset-root");
        std::fs::create_dir_all(&root)?;
        let blob = OpendalBlob::fs(&root, None).map_err(std::io::Error::other)?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let res = rt.block_on(blob.replace_atomic("k", b"v".to_vec()));
        let note = match res {
            Err(BlobError::Unsupported(m)) => m,
            other => format!("expected Unsupported, got {other:?}"),
        };
        rows.push(Row {
            subject_kind: "capability".to_string(),
            subject: id.clone(),
            backend: "opendal-fs".to_string(),
            version: version.clone(),
            config: blob.config().clone(),
            enabled: false,
            reason: Some(Reason::Missing),
            metadata: env.clone().with("test", "crash::replace_atomic"),
            note,
            extra: Default::default(),
        });
    }

    // atomic_write_dir set: kill the target, and both controls.
    let run_mode = |mode: Mode, name: &str| -> std::io::Result<Report> {
        let root = work.join(format!("{name}-root"));
        let scratch = work.join(format!("{name}-scratch"));
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(&scratch)?;
        run(mode, &root, &scratch, child_args)
    };
    let positive = run_mode(Mode::Rename, "rename")?;
    let known_bad = run_mode(Mode::InPlace, "inplace")?;
    let target = run_mode(Mode::Opendal, "opendal")?;
    let controls = Controls {
        positive_passed: positive.torn == 0,
        known_bad_failed: known_bad.torn > 0,
    };
    let verdict = if target.torn == 0 {
        Verdict::Exact
    } else {
        Verdict::Wrong
    };
    let mut metadata = env
        .clone()
        .with("test", "crash::replace_atomic")
        .with("rounds", ROUNDS.to_string())
        .with("value_bytes", LEN.to_string())
        .with("kill", "hard");
    if verdict == Verdict::Wrong {
        metadata = metadata.with("torn", target.torn.to_string());
    }
    let config = Config::new().with("atomic_write_dir", "set");
    let row = match verdict.into_row(
        id.clone(),
        "opendal-fs",
        version.clone(),
        config.clone(),
        metadata.clone(),
        controls,
    ) {
        Ok(r) => r,
        Err(_) => Row {
            subject_kind: "capability".to_string(),
            subject: id,
            backend: "opendal-fs".to_string(),
            version,
            config,
            enabled: false,
            reason: Some(Reason::Untested),
            metadata,
            note: format!("controls did not hold: {controls:?}"),
            extra: Default::default(),
        },
    };
    rows.push(row);
    Ok(rows)
}
