// SPDX-License-Identifier: MIT OR Apache-2.0
//! Verifying the **`Process`** durability class: a `put` that returned `Ok` is still there after the
//! writing process is killed hard and the store is opened again.
//!
//! A host binary re-runs **itself** as the writer child ([`maybe_run_child`] first thing in `main`).
//! The child opens the backend and writes numbered keys, printing `acked N` after each `put` returns.
//! The parent reads the acknowledgements, kills the child (no destructor, no flush), opens the store
//! again through the caller's `reopen`, and checks that every acknowledged key holds its value.
//!
//! **Known-bad control.** The same child can run in [`Mode::AckFirst`]: it prints `acked N` *before*
//! the `put` and then waits, so the kill lands between the lie and the write. The verifier must find
//! those keys missing, or it proves nothing about the honest run. [`certify_process`] records the leaf
//! **on** only when the honest run lost nothing *and* the lying run was caught.
//!
//! **What this does not test.** Power loss and operating-system crashes (data acknowledged but still in
//! the page cache or a disk cache): a process kill leaves the OS and its caches intact. That is the
//! `Machine` class, which needs a VM hard reset and stays *declared*
//! ([`choose::declared_durability`](crate::choose::declared_durability)). A memory store run through
//! this honestly **fails** it (nothing survives the process), which is the volatile class working as
//! documented.

use crate::{KvBackend, KvError};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

/// The environment variable that makes a host binary behave as the writer child.
pub const ENV: &str = "PERSISTANT_DURABILITY_CHILD";

/// The capability id this module decides.
pub const LEAF: &str = "kv.durability.process";

const MAX_KEYS: u32 = 5_000;
const LIE_PAUSE: Duration = Duration::from_millis(400);

/// How the child acknowledges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Acknowledge after `put` returns.
    Honest,
    /// Acknowledge before `put`, then wait: the known-bad control.
    AckFirst,
}

impl Mode {
    fn word(self) -> &'static str {
        match self {
            Mode::Honest => "honest",
            Mode::AckFirst => "ackfirst",
        }
    }
}

/// The key the child writes as its `n`th. The mode is part of the key, so an honest run and a lying
/// run can share one store without the lying run being satisfied by the honest run's keys.
pub fn key(mode: Mode, n: u32) -> String {
    format!("persistant-durability/{}/{n:06}", mode.word())
}

/// The value the child writes for key `n`: recognisable and not all one byte.
pub fn value(n: u32) -> Vec<u8> {
    let mut v = vec![0xD7_u8; 96];
    v[..4].copy_from_slice(&n.to_be_bytes());
    v
}

/// If this process was started as the writer child, run `run(spec)` (which must open the backend and
/// call [`child_loop`]) and exit. Otherwise return at once. Call it first thing in `main`, before any
/// runtime exists.
pub fn maybe_run_child(run: impl FnOnce(&str, Mode)) {
    let Ok(raw) = std::env::var(ENV) else {
        return;
    };
    let (mode, spec) = raw.split_once('|').expect("bad durability child spec");
    let mode = match mode {
        "honest" => Mode::Honest,
        "ackfirst" => Mode::AckFirst,
        other => panic!("unknown durability mode {other}"),
    };
    run(spec, mode);
    std::process::exit(0);
}

/// The child's body: write numbered keys and print `acked N` as `mode` says. Returns after
/// `MAX_KEYS` (the parent normally kills it long before).
pub async fn child_loop<K: KvBackend>(kv: &K, mode: Mode) -> Result<(), KvError> {
    let say = |n: u32| {
        println!("acked {n}");
        let _ = std::io::stdout().flush();
    };
    for n in 0..MAX_KEYS {
        match mode {
            Mode::Honest => {
                kv.put(&key(mode, n), value(n), None).await?;
                say(n);
            }
            Mode::AckFirst => {
                say(n);
                // Long enough that the parent's kill lands before the write.
                std::thread::sleep(LIE_PAUSE);
                kv.put(&key(mode, n), value(n), None).await?;
            }
        }
    }
    Ok(())
}

/// Remove every key a run of this module wrote, from a store that outlives the test (a server).
pub async fn cleanup<K: KvBackend>(kv: &K) -> Result<(), KvError> {
    loop {
        let page = kv.prefix_list("persistant-durability/", None, 500).await?;
        if page.keys.is_empty() {
            return Ok(());
        }
        for k in &page.keys {
            kv.delete(k).await?;
        }
    }
}

/// What a kill found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Keys the child acknowledged before it was killed.
    pub acked: u32,
    /// Acknowledged keys that were absent or held another value afterwards (first few).
    pub lost: Vec<u32>,
}

impl Outcome {
    /// Nothing acknowledged was lost, and something was acknowledged (an empty run proves nothing).
    pub fn held(&self) -> bool {
        self.acked > 0 && self.lost.is_empty()
    }
}

/// Run the child in `mode` with `spec`, kill it once it has acknowledged `kill_after` keys, reopen the
/// store with `reopen` and check every acknowledged key.
pub async fn verify<K, F, Fut>(
    spec: &str,
    mode: Mode,
    kill_after: u32,
    reopen: F,
) -> Result<Outcome, KvError>
where
    K: KvBackend,
    F: FnOnce() -> Fut,
    Fut: Future<Output = K>,
{
    let exe = std::env::current_exe().map_err(|e| KvError::Backend(e.to_string()))?;
    let mut child = Command::new(exe)
        .env(ENV, format!("{}|{spec}", mode.word()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| KvError::Backend(format!("cannot start the writer child: {e}")))?;
    let out = child.stdout.take().expect("piped");
    let mut acked = 0_u32;
    for line in BufReader::new(out).lines() {
        let Ok(line) = line else { break };
        if let Some(n) = line
            .strip_prefix("acked ")
            .and_then(|n| n.parse::<u32>().ok())
        {
            acked = n + 1;
            if acked >= kill_after {
                break;
            }
        }
    }
    child
        .kill()
        .map_err(|e| KvError::Backend(format!("cannot kill the writer child: {e}")))?;
    let _ = child.wait();

    let kv = reopen().await;
    let mut lost = Vec::new();
    for n in 0..acked {
        if kv.get(&key(mode, n)).await?.as_deref() != Some(value(n).as_slice()) {
            lost.push(n);
            if lost.len() >= 8 {
                break;
            }
        }
    }
    Ok(Outcome { acked, lost })
}

/// What the matrix row is about.
#[derive(Debug, Clone)]
pub struct Target<'a> {
    /// Backend name, as in the capability matrix.
    pub backend: &'a str,
    /// Backend version.
    pub version: &'a str,
    /// Backend configuration.
    pub config: &'a Config,
    /// Facts copied into the row's metadata.
    pub metadata: &'a Config,
}

/// One child run: the storage it is told about and how the parent opens that storage again.
pub struct Run<'a, F> {
    /// Handed to the child (a path, a URL): the child's `run(spec, mode)` opens the backend from it.
    pub spec: &'a str,
    /// Opens the same storage again in the parent, after the kill.
    pub reopen: F,
}

/// Decide `kv.durability.process` for a store: the honest run must lose nothing and the lying run
/// must be caught. The runs may share a store (their keys differ by mode).
pub async fn certify_process<K, F, G, FutF, FutG>(
    target: &Target<'_>,
    honest: Run<'_, F>,
    lying: Run<'_, G>,
) -> Row
where
    K: KvBackend,
    F: FnOnce() -> FutF,
    FutF: Future<Output = K>,
    G: FnOnce() -> FutG,
    FutG: Future<Output = K>,
{
    let Target {
        backend,
        version,
        config,
        metadata,
    } = target;
    const KILL_AFTER: u32 = 20;
    let md = (*metadata)
        .clone()
        .with("test", format!("kv::{LEAF}"))
        .with("kill_after", KILL_AFTER.to_string());
    let id = CapabilityId::new(LEAF).expect("leaf id is valid");
    let off = |reason: Reason, note: String| Row {
        subject_kind: "capability".to_string(),
        subject: id.clone(),
        backend: (*backend).to_string(),
        version: (*version).to_string(),
        config: (*config).clone(),
        enabled: false,
        reason: Some(reason),
        metadata: md.clone(),
        note: note.replace(['\t', '\n'], " "),
        extra: Default::default(),
    };

    let honest = match verify(honest.spec, Mode::Honest, KILL_AFTER, honest.reopen).await {
        Ok(o) => o,
        Err(e) => return off(Reason::Missing, e.to_string()),
    };
    let lying = match verify(lying.spec, Mode::AckFirst, 3, lying.reopen).await {
        Ok(o) => o,
        Err(e) => return off(Reason::Missing, e.to_string()),
    };
    let controls = Controls {
        // The harness acknowledged and checked something: the honest run was not vacuous.
        positive_passed: honest.acked >= KILL_AFTER,
        // The lying child's acknowledged-but-unwritten keys were found missing.
        known_bad_failed: lying.acked > 0 && !lying.lost.is_empty(),
    };
    if !honest.lost.is_empty() {
        return off(
            Reason::Wrong,
            format!(
                "{} of {} acknowledged keys were not there after the writer was killed (first: {:?})",
                honest.lost.len(),
                honest.acked,
                honest.lost
            ),
        );
    }
    match Verdict::Exact.into_row(
        id.clone(),
        *backend,
        *version,
        (*config).clone(),
        md.clone(),
        controls,
    ) {
        Ok(row) => row,
        Err(_) => off(
            Reason::Untested,
            format!("controls did not hold: {controls:?} (honest {honest:?}, lying {lying:?})"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_distinct_and_keys_sort_in_write_order() {
        assert_ne!(value(1), value(2));
        assert_eq!(value(7).len(), 96);
        assert!(key(Mode::Honest, 9) < key(Mode::Honest, 10));
        assert_ne!(key(Mode::Honest, 1), key(Mode::AckFirst, 1));
    }

    #[test]
    fn an_empty_run_does_not_count_as_held() {
        assert!(
            !Outcome {
                acked: 0,
                lost: vec![]
            }
            .held()
        );
        assert!(
            Outcome {
                acked: 3,
                lost: vec![]
            }
            .held()
        );
        assert!(
            !Outcome {
                acked: 3,
                lost: vec![2]
            }
            .held()
        );
    }
}
