// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `text.index` test (feature `text-index`): [`crate::text::TextIndex`] over a kv backend
//! keeps its postings, document lengths and corpus totals equal to what a plain map held by the
//! test says, after every operation of a scripted sequence of puts, replaces and removals, and
//! under concurrent writers.
//!
//! [`certify`] runs the checks on the target, on a correct reference (the positive control: the
//! test can pass) and on reference backends with a [`Defect`] (the known-bad control: the test can
//! fail). The row is **on** only when the target passed and both controls behaved; a backend with
//! no atomic batch says `Unsupported` and is recorded **off as missing**.
//!
//! The concurrent checks run the writers on one task, with a yield before every read and every
//! batch, so every writer reads before any writes: that is what makes a batch that ignores its
//! `Require` visible on the memory reference.

use crate::text::{Match, TextIndex};
use crate::{BatchOp, Defect, KeyPage, KvBackend, KvError, MemoryKv, ValuePage};
use persistant_blob::concurrency::{Task, join_all, yield_now};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::collections::{BTreeMap, BTreeSet};

/// Writers racing in the concurrent checks.
const WRITERS: usize = 8;
/// Rounds of each concurrent check.
const ROUNDS: usize = 5;
/// Steps of the scripted sequence.
const STEPS: usize = 30;

/// Why the check did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The index returned something other than the contract says.
    Wrong(String),
    /// The backend could not perform an operation the test needs.
    Backend(KvError),
}

impl From<KvError> for Failure {
    fn from(e: KvError) -> Self {
        Failure::Backend(e)
    }
}

fn wrong<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Wrong(msg.into()))
}

/// A view of a backend that yields to the other writers before every read and every batch.
struct Yielding<'a, K>(&'a K);

impl<K: KvBackend> KvBackend for Yielding<'_, K> {
    fn backend(&self) -> &str {
        self.0.backend()
    }
    fn version(&self) -> &str {
        self.0.version()
    }
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        yield_now().await;
        self.0.get(key).await
    }
    async fn put(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), KvError> {
        self.0.put(key, value, ttl).await
    }
    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.delete(key).await
    }
    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<std::time::Duration>,
    ) -> Result<bool, KvError> {
        self.0.put_if_absent(key, value, ttl).await
    }
    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.0.ordered_scan(prefix, after, limit).await
    }
    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        yield_now().await;
        self.0.batch(ops).await
    }
    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        self.0.ordered_scan_values(prefix, after, limit).await
    }
}

type Terms = BTreeMap<String, u32>;
type Oracle = BTreeMap<String, Terms>;

const VOCAB: [&str; 6] = ["alpha", "beta", "gamma", "a", "ab", "abc"];

/// A tiny deterministic generator, so a failure reproduces.
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

fn pairs(t: &Terms) -> Vec<(String, u32)> {
    t.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

/// Every observable of the index against the oracle.
async fn agree<K: KvBackend>(
    ix: &TextIndex<'_, K>,
    name: &str,
    oracle: &Oracle,
    step: usize,
) -> Result<(), Failure> {
    let stats = ix.stats(name).await?;
    let want_len: u64 = oracle
        .values()
        .map(|t| t.values().map(|v| u64::from(*v)).sum::<u64>())
        .sum();
    if (stats.docs, stats.total_len) != (oracle.len() as u64, want_len) {
        return wrong(format!(
            "step {step}: totals are ({}, {}), want ({}, {want_len})",
            stats.docs,
            stats.total_len,
            oracle.len()
        ));
    }
    for (doc, terms) in oracle {
        let want: u32 = terms.values().sum();
        if ix.doc_len(name, doc).await? != Some(want) {
            return wrong(format!("step {step}: the length of {doc} is not {want}"));
        }
    }
    let mut postings = 0;
    for term in VOCAB {
        let want: Vec<(String, u32)> = oracle
            .iter()
            .filter_map(|(d, t)| t.get(term).map(|tf| (d.clone(), *tf)))
            .collect();
        postings += want.len();
        let (mut got, mut after) = (Vec::new(), None::<String>);
        loop {
            let page = ix.postings(name, term, after.as_deref(), 2).await?;
            got.extend(page.entries);
            match page.next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
        if got != want {
            return wrong(format!(
                "step {step}: postings of {term:?} are {got:?}, want {want:?}"
            ));
        }
    }
    // Nothing stale: the keys under the posting prefix are exactly the oracle's postings.
    let prefix = format!("tx\u{1f}{name}\u{1f}");
    let (mut keys, mut after) = (0, None::<String>);
    loop {
        let page = ix.kv().ordered_scan(&prefix, after.as_deref(), 100).await?;
        keys += page.keys.len();
        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    if keys != postings {
        return wrong(format!(
            "step {step}: {keys} posting keys are stored, the documents have {postings}"
        ));
    }
    let (mut ids, mut after) = (Vec::new(), None::<String>);
    loop {
        let page = ix.docs(name, after.as_deref(), 2).await?;
        ids.extend(page.ids);
        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    if ids != oracle.keys().cloned().collect::<Vec<_>>() {
        return wrong(format!("step {step}: the document list is {ids:?}"));
    }
    let q: Vec<String> = ["alpha", "a"].iter().map(|s| s.to_string()).collect();
    for mode in [Match::Any, Match::All] {
        let mut want: Vec<(String, u64)> = oracle
            .iter()
            .filter_map(|(d, t)| {
                let held: Vec<u32> = q.iter().filter_map(|w| t.get(w).copied()).collect();
                let ok = match mode {
                    Match::Any => !held.is_empty(),
                    Match::All => held.len() == q.len(),
                };
                ok.then(|| (d.clone(), held.iter().map(|x| u64::from(*x)).sum()))
            })
            .collect();
        want.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let got: Vec<(String, u64)> = ix
            .search(name, &q, mode, 100)
            .await?
            .into_iter()
            .map(|h| (h.doc_id, h.score))
            .collect();
        if got != want {
            return wrong(format!(
                "step {step}: {mode:?} search is {got:?}, want {want:?}"
            ));
        }
    }
    Ok(())
}

async fn sequence<K: KvBackend>(kv: &K, base: &str) -> Result<(), Failure> {
    let ix = TextIndex::new(kv);
    let name = format!("{base}seq");
    let mut oracle = Oracle::new();
    let mut g = Lcg(7);
    let docs = ["d1", "d2", "d3"];
    agree(&ix, &name, &oracle, 0).await?;
    for step in 1..=STEPS {
        let doc = docs[g.below(docs.len())];
        if g.below(4) == 0 {
            let removed = ix.remove_many(&name, &[doc]).await?;
            if removed != usize::from(oracle.remove(doc).is_some()) {
                return wrong(format!(
                    "step {step}: remove_many of {doc} returned {removed}"
                ));
            }
        } else {
            let mut t = Terms::new();
            for _ in 0..1 + g.below(3) {
                *t.entry(VOCAB[g.below(VOCAB.len())].to_string())
                    .or_insert(0) += 1 + g.below(3) as u32;
            }
            ix.put(&name, doc, &pairs(&t)).await?;
            oracle.insert(doc.to_string(), t);
        }
        agree(&ix, &name, &oracle, step).await?;
    }
    ix.remove_many(&name, &docs).await?;
    agree(&ix, &name, &Oracle::new(), STEPS + 1).await
}

/// Writers of different documents at once: every one is counted and the length is the sum.
async fn concurrent_documents<K: KvBackend>(kv: &K, base: &str) -> Result<(), Failure> {
    let owned = Yielding(kv);
    let view = &owned;
    for round in 0..ROUNDS {
        let name = format!("{base}docs{round}");
        let ix = TextIndex::new(view);
        let tasks: Vec<Task<'_, Result<(), KvError>>> = (0..WRITERS)
            .map(|i| -> Task<'_, _> {
                let name = name.clone();
                Box::pin(async move {
                    TextIndex::new(view)
                        .put(&name, &format!("d{i}"), &[("alpha".into(), 1 + i as u32)])
                        .await
                })
            })
            .collect();
        for r in join_all(tasks).await {
            r?;
        }
        let s = ix.stats(&name).await?;
        let want = (1..=WRITERS as u64).sum::<u64>();
        if (s.docs, s.total_len) != (WRITERS as u64, want) {
            return wrong(format!(
                "round {round}: {WRITERS} concurrent writers left totals ({}, {}), want ({WRITERS}, {want})",
                s.docs, s.total_len
            ));
        }
        let n = ix.postings(&name, "alpha", None, 100).await?.entries.len();
        if n != WRITERS {
            return wrong(format!("round {round}: {n} postings, want {WRITERS}"));
        }
        ix.remove_many(
            &name,
            &(0..WRITERS)
                .map(|i| format!("d{i}"))
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
        )
        .await?;
    }
    Ok(())
}

/// Writers of one document at once, each with a different text: the postings are those of one.
async fn concurrent_same_document<K: KvBackend>(kv: &K, base: &str) -> Result<(), Failure> {
    let owned = Yielding(kv);
    let view = &owned;
    for round in 0..ROUNDS {
        let name = format!("{base}same{round}");
        let tasks: Vec<Task<'_, Result<(), KvError>>> = (0..WRITERS)
            .map(|i| -> Task<'_, _> {
                let name = name.clone();
                Box::pin(async move {
                    TextIndex::new(view)
                        .put(&name, "same", &[(format!("w{i}"), 3)])
                        .await
                })
            })
            .collect();
        for r in join_all(tasks).await {
            r?;
        }
        let ix = TextIndex::new(view);
        let mut holders = BTreeSet::new();
        for i in 0..WRITERS {
            if !ix
                .postings(&name, &format!("w{i}"), None, 10)
                .await?
                .entries
                .is_empty()
            {
                holders.insert(i);
            }
        }
        let s = ix.stats(&name).await?;
        if holders.len() != 1 || (s.docs, s.total_len) != (1, 3) {
            return wrong(format!(
                "round {round}: postings of {holders:?} survived and the totals are ({}, {}), want one text and (1, 3)",
                s.docs, s.total_len
            ));
        }
        ix.remove_many(&name, &["same"]).await?;
    }
    Ok(())
}

/// Run the `text.index` checks against `kv`. Writes only under indexes named from `base`.
pub async fn run<K: KvBackend>(kv: &K, base: &str) -> Result<(), Failure> {
    sequence(kv, base).await?;
    concurrent_documents(kv, base).await?;
    concurrent_same_document(kv, base).await
}

/// Whether `TextIndex::remove_many_erased` runs on `kv` at all, for a matrix generator that then
/// either reads the files back (SQLite) or records the leaf off: `Ok(None)` if it ran; `Ok(Some(why))`
/// if the backend said `Unsupported` (the note for the row); `Err` for any other failure. Leaves nothing
/// behind under indexes named from `base`.
pub async fn erased_unsupported<K: KvBackend>(
    kv: &K,
    base: &str,
) -> Result<Option<String>, KvError> {
    let ix = TextIndex::new(kv);
    let name = format!("{base}erased-probe");
    match ix.put(&name, "d", &[("probe".to_string(), 1)]).await {
        Ok(()) => {}
        Err(KvError::Unsupported(m)) => {
            return Ok(Some(format!("kv backend does not support: {m}")));
        }
        Err(e) => return Err(e),
    }
    match ix.remove_many_erased(&name, &["d"]).await {
        Ok(_) => Ok(None),
        Err(KvError::Unsupported(m)) => {
            ix.remove_many(&name, &["d"]).await?;
            Ok(Some(format!("kv backend does not support: {m}")))
        }
        Err(e) => Err(e),
    }
}

/// The reference backends that must fail the test: a batch that ignores its `Require`, and a batch
/// that is not applied as a unit.
const DEFECTS: [Defect; 2] = [Defect::BatchIgnoresRequire, Defect::BatchNotAtomic];

/// Certify `text.index` for `target`, recorded under `backend` and `version`.
///
/// `config` is the facts the gate can read for this target; `env` is copied into the row's metadata
/// with `test=text::text.index` and the trial parameters added.
pub async fn certify<K: KvBackend>(
    target: &K,
    backend: &str,
    version: &str,
    config: &Config,
    env: &Config,
) -> Row {
    let base = "persistant-conformance/text.index/";
    let positive_passed = run(&MemoryKv::new(), base).await.is_ok();
    let mut known_bad_failed = true;
    for d in DEFECTS {
        if run(&MemoryKv::with_defect(d), base).await.is_ok() {
            known_bad_failed = false;
        }
    }
    let controls = Controls {
        positive_passed,
        known_bad_failed,
    };
    let metadata = env
        .clone()
        .with("test", "text::text.index")
        .with("writers", WRITERS.to_string())
        .with("rounds", ROUNDS.to_string())
        .with("steps", STEPS.to_string());
    let id = CapabilityId::new("text.index").expect("valid id");
    let off = |reason: Reason, note: String| Row {
        subject_kind: "capability".to_string(),
        subject: id.clone(),
        backend: backend.to_string(),
        version: version.to_string(),
        config: config.clone(),
        enabled: false,
        reason: Some(reason),
        metadata: metadata.clone(),
        note: note.replace(['\t', '\n'], " "),
        extra: BTreeMap::new(),
    };
    match run(target, base).await {
        Ok(()) if positive_passed && known_bad_failed => Verdict::Exact
            .into_row(
                id.clone(),
                backend,
                version,
                config.clone(),
                metadata.clone(),
                controls,
            )
            .expect("both controls hold"),
        Ok(()) => off(
            Reason::Untested,
            format!("controls did not hold: {controls:?}"),
        ),
        Err(Failure::Wrong(m)) => off(Reason::Wrong, m),
        Err(Failure::Backend(KvError::Unsupported(m))) => {
            off(Reason::Missing, format!("kv backend does not support: {m}"))
        }
        Err(Failure::Backend(e)) => off(Reason::Missing, e.to_string()),
    }
}
