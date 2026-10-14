// SPDX-License-Identifier: MIT OR Apache-2.0
//! Contract tests for the blob leaves whose written contract sequential calls can decide.
//!
//! [`certify`] runs each leaf's test on the target, on a correct reference backend (the positive
//! control: the test can pass) and on reference backends with a [`Defect`] (the known-bad control:
//! the test can fail). A leaf is recorded **on** only when the target passed and both controls
//! behaved. Anything else is an off row with the reason.

use crate::{BlobBackend, BlobError, Defect, MemoryBlob};
use persistant_core::{CapabilityId, Config, Controls, Reason, Row, Verdict};
use std::collections::BTreeMap;

/// A leaf this module can decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Leaf {
    /// `blob.read`
    Read,
    /// `blob.write`
    Write,
    /// `blob.delete`
    Delete,
    /// `blob.stat`
    Stat,
    /// `blob.list`
    List,
}

impl Leaf {
    /// Every leaf with a test here.
    pub const ALL: [Leaf; 5] = [
        Leaf::Read,
        Leaf::Write,
        Leaf::Delete,
        Leaf::Stat,
        Leaf::List,
    ];

    /// The capability id.
    pub fn id(self) -> &'static str {
        match self {
            Leaf::Read => "blob.read",
            Leaf::Write => "blob.write",
            Leaf::Delete => "blob.delete",
            Leaf::Stat => "blob.stat",
            Leaf::List => "blob.list",
        }
    }

    fn defects(self) -> &'static [Defect] {
        match self {
            Leaf::Read => &[Defect::ReadTruncates],
            Leaf::Write => &[Defect::WriteIsLost],
            Leaf::Delete => &[Defect::DeleteIsIgnored],
            Leaf::Stat => &[Defect::StatLiesAboutLength],
            Leaf::List => &[
                Defect::ListIsDescending,
                Defect::ListPrefixIsGlob,
                Defect::ListIgnoresPaging,
            ],
        }
    }
}

/// Why a leaf's test did not pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The backend returned something other than the contract says.
    Wrong(String),
    /// The backend could not perform an operation the test needs.
    Backend(BlobError),
}

impl From<BlobError> for Failure {
    fn from(e: BlobError) -> Self {
        Failure::Backend(e)
    }
}

fn wrong<T>(msg: impl Into<String>) -> Result<T, Failure> {
    Err(Failure::Wrong(msg.into()))
}

/// Run one leaf's test against `b`. Writes only under `persistant-conformance/<leaf>/`.
pub async fn run_leaf<B: BlobBackend>(leaf: Leaf, b: &B) -> Result<(), Failure> {
    let base = format!("persistant-conformance/{}/", leaf.id());
    let result = match leaf {
        Leaf::Read => read(b, &base).await,
        Leaf::Write => write(b, &base).await,
        Leaf::Delete => delete(b, &base).await,
        Leaf::Stat => stat(b, &base).await,
        Leaf::List => list(b, &base).await,
    };
    // Best-effort cleanup so a shared target is left as found.
    if let Ok(page) = b.list(&base, None, usize::MAX).await {
        for e in page.entries {
            let _ = b.delete(&e.key).await;
        }
    }
    result
}

async fn read<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    if b.read(&k).await?.is_some() {
        return wrong("an absent key read as present");
    }
    for value in [vec![], vec![0u8], vec![1, 2, 3, 0xFF, 0x00, 0x80]] {
        b.write(&k, value.clone()).await?;
        match b.read(&k).await? {
            Some(got) if got == value => {}
            Some(got) => return wrong(format!("wrote {value:?}, read {got:?}")),
            None => return wrong(format!("wrote {value:?}, read nothing")),
        }
    }
    Ok(())
}

async fn write<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}nested/dir/k");
    b.write(&k, b"first".to_vec()).await?;
    b.write(&k, b"second, longer".to_vec()).await?;
    match b.read(&k).await? {
        Some(v) if v == b"second, longer" => {}
        other => return wrong(format!("after two writes read {other:?}")),
    }
    b.write(&k, b"x".to_vec()).await?;
    match b.read(&k).await? {
        Some(v) if v == b"x" => Ok(()),
        other => wrong(format!("a shorter replacement left {other:?}")),
    }
}

async fn delete<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    b.write(&k, b"v".to_vec()).await?;
    b.delete(&k).await?;
    if b.read(&k).await?.is_some() {
        return wrong("a deleted key still reads");
    }
    b.delete(&k).await?; // deleting an absent key is not an error
    Ok(())
}

async fn stat<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    let k = format!("{base}k");
    if b.stat(&k).await?.is_some() {
        return wrong("an absent key has a stat");
    }
    for len in [0usize, 1, 5, 4096] {
        b.write(&k, vec![7u8; len]).await?;
        match b.stat(&k).await? {
            Some(e) if e.key == k && e.len == len as u64 => {}
            other => return wrong(format!("a {len}-byte value stat as {other:?}")),
        }
    }
    Ok(())
}

async fn list<B: BlobBackend>(b: &B, base: &str) -> Result<(), Failure> {
    // "1" < "10" < "2" bytewise: a numeric or natural sort would put "2" before "10".
    let under = ["a/1", "a/10", "a/2", "a/b/c"];
    for k in under.iter().chain(["ab", "a*z", "b/1"].iter()) {
        b.write(&format!("{base}{k}"), k.as_bytes().to_vec())
            .await?;
    }
    let full = |s: &str| format!("{base}{s}");
    let want: Vec<String> = ["a/1", "a/10", "a/2", "a/b/c"]
        .iter()
        .map(|s| full(s))
        .collect();

    // One page: literal prefix, ascending bytewise, complete.
    let all = b.list(&full("a/"), None, 100).await?;
    let got: Vec<String> = all.entries.iter().map(|e| e.key.clone()).collect();
    if got != want || all.next.is_some() {
        return wrong(format!(
            "prefix a/ listed {got:?} next {:?}, want {want:?}",
            all.next
        ));
    }
    if all.entries.iter().any(|e| e.len == 0) {
        return wrong("a listed entry reports length 0 for a non-empty value");
    }

    // The prefix is literal: `*` matches only itself.
    let star = b.list(&full("a*"), None, 100).await?;
    let got: Vec<String> = star.entries.iter().map(|e| e.key.clone()).collect();
    if got != vec![full("a*z")] {
        return wrong(format!(
            "prefix a* listed {got:?}, want only {:?}",
            full("a*z")
        ));
    }

    // Paging: limit 2 over 4 keys, each key once, continuation ends with None.
    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..10 {
        let p = b.list(&full("a/"), after.as_deref(), 2).await?;
        if p.entries.len() > 2 {
            return wrong(format!(
                "a page of limit 2 held {} entries",
                p.entries.len()
            ));
        }
        seen.extend(p.entries.iter().map(|e| e.key.clone()));
        match p.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    if seen != want {
        return wrong(format!("paged listing returned {seen:?}, want {want:?}"));
    }
    Ok(())
}

/// The outcome of certifying one leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Certified {
    /// The leaf.
    pub leaf: Leaf,
    /// The row to record.
    pub row: Row,
}

/// Run every leaf here against `target` and return one row per leaf.
///
/// `config` is the facts the gate can read for this target (`collation=...`); `metadata` is
/// copied into every row, with `test=blob::<leaf>` added. A leaf is **on** only if the target
/// passed *and* the positive control passed *and* every known-bad control failed.
pub async fn certify<B: BlobBackend>(
    target: &B,
    config: &Config,
    metadata: &Config,
) -> Vec<Certified> {
    let mut out = Vec::new();
    for leaf in Leaf::ALL {
        let positive_passed = run_leaf(leaf, &MemoryBlob::new()).await.is_ok();
        let mut known_bad_failed = true;
        for d in leaf.defects() {
            if run_leaf(leaf, &MemoryBlob::with_defect(*d)).await.is_ok() {
                known_bad_failed = false;
            }
        }
        let controls = Controls {
            positive_passed,
            known_bad_failed,
        };
        let result = run_leaf(leaf, target).await;
        let md = metadata
            .clone()
            .with("test", format!("blob::{}", leaf.id()));
        let id = CapabilityId::new(leaf.id()).expect("leaf id is valid");
        let row = match (
            &result,
            controls.positive_passed && controls.known_bad_failed,
        ) {
            (Ok(()), true) => Verdict::Exact
                .into_row(
                    id,
                    target.backend(),
                    target.version(),
                    config.clone(),
                    md,
                    controls,
                )
                .expect("both controls hold"),
            (Ok(()), false) => off(
                id,
                target,
                config,
                md,
                Reason::Untested,
                format!("controls did not hold: {controls:?}"),
            ),
            (Err(Failure::Wrong(m)), _) => off(id, target, config, md, Reason::Wrong, m.clone()),
            (Err(Failure::Backend(e)), _) => {
                off(id, target, config, md, Reason::Missing, e.to_string())
            }
        };
        out.push(Certified { leaf, row });
    }
    out
}

fn off<B: BlobBackend>(
    subject: CapabilityId,
    target: &B,
    config: &Config,
    metadata: Config,
    reason: Reason,
    note: String,
) -> Row {
    Row {
        subject_kind: "capability".to_string(),
        subject,
        backend: target.backend().to_string(),
        version: target.version().to_string(),
        config: config.clone(),
        enabled: false,
        reason: Some(reason),
        metadata,
        note: note.replace(['\t', '\n'], " "),
        extra: BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_correct_backend_is_on_for_every_leaf() {
        let rows = certify(&MemoryBlob::new(), &Config::new(), &Config::new()).await;
        assert_eq!(rows.len(), Leaf::ALL.len());
        for c in &rows {
            assert!(c.row.enabled, "{}: {}", c.leaf.id(), c.row.note);
            assert!(c.row.metadata.get("test").is_some());
        }
    }

    #[tokio::test]
    async fn every_defect_turns_its_own_leaf_off_as_wrong() {
        for leaf in Leaf::ALL {
            for d in leaf.defects() {
                let rows =
                    certify(&MemoryBlob::with_defect(*d), &Config::new(), &Config::new()).await;
                let mine = rows.iter().find(|c| c.leaf == leaf).unwrap();
                assert!(!mine.row.enabled, "{d:?} must turn {} off", leaf.id());
                assert_eq!(mine.row.reason, Some(Reason::Wrong), "{d:?}");
            }
        }
    }

    #[tokio::test]
    async fn the_harness_can_tell_leaves_apart() {
        // Control: ReadTruncates breaks blob.read and leaves blob.delete on.
        let rows = certify(
            &MemoryBlob::with_defect(Defect::ReadTruncates),
            &Config::new(),
            &Config::new(),
        )
        .await;
        let by = |l: Leaf| rows.iter().find(|c| c.leaf == l).unwrap().row.enabled;
        assert!(!by(Leaf::Read));
        assert!(by(Leaf::Stat), "a read defect must not turn stat off");
    }

    #[tokio::test]
    async fn rows_round_trip_through_the_matrix_and_the_gate_reads_them() {
        let rows: Vec<Row> = certify(&MemoryBlob::new(), &Config::new(), &Config::new())
            .await
            .into_iter()
            .map(|c| c.row)
            .collect();
        let m = persistant_core::Matrix::parse(&persistant_core::Matrix::from_rows(rows).to_tsv())
            .unwrap();
        let id = CapabilityId::new("blob.list").unwrap();
        let v = env!("CARGO_PKG_VERSION");
        assert!(m.enabled(&id, "memory", v, &Config::new()));
        assert!(
            !m.enabled(&id, "memory", "0.0.0-other", &Config::new()),
            "unknown version is off"
        );
    }
}
