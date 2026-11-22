// SPDX-License-Identifier: MIT OR Apache-2.0
//! A migration ledger for stores with no SQL (ROADMAP row 19, `docs/DESIGN-NONSQL-HELPERS.md`): the
//! steps that change a directory layout or a file format, run once each, by one migrator at a time.
//!
//! Per step `<id>` two keys under the ledger's prefix: `<id>.claim`, made with
//! [`BlobBackend::create_exclusive`] (one winner among any number of migrators), and `<id>.done`,
//! written after the step returned `Ok`. A step that returns an error releases its own claim, so it can
//! run again. **The limit, by name:** a migrator that dies between the claim and the done marker leaves a
//! claim nobody can take over safely (taking over has the same delete race as a lease, and a blob store
//! has no compare-and-delete), so the next run reports [`StepOutcome::Claimed`] with who and when, and
//! the operator removes the `.claim` after checking the store. Steps must be idempotent.

use crate::{BlobBackend, BlobError};
use std::future::Future;

/// What [`Ledger::run`] did with a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// This call won the claim, ran the step and recorded it done.
    Ran,
    /// An earlier run finished this step; nothing was run.
    AlreadyDone,
    /// Another migrator holds the claim and has not finished: running now, or died. The claim's text
    /// (`<when> <who>`) says who claimed it and when.
    Claimed(String),
}

/// Why [`Ledger::run`] did not finish.
#[derive(Debug)]
pub enum LedgerError<E> {
    /// The blob store refused or failed.
    Store(BlobError),
    /// The step itself failed; its claim was released.
    Step(E),
    /// The step id is empty or has a `/` in it.
    BadId,
}

/// A ledger of migration steps kept under `prefix` in a blob store.
#[derive(Debug)]
pub struct Ledger<'a, B> {
    backend: &'a B,
    prefix: String,
}

impl<'a, B: BlobBackend> Ledger<'a, B> {
    /// A ledger under `prefix` (for example `"migrations/"`).
    pub fn new(backend: &'a B, prefix: &str) -> Self {
        Ledger {
            backend,
            prefix: prefix.to_string(),
        }
    }

    fn key(&self, id: &str, suffix: &str) -> String {
        format!("{}{id}.{suffix}", self.prefix)
    }

    /// Run step `id` once. `who` and `when` are recorded in the claim for the operator (a host and pid,
    /// a timestamp); the ledger reads no clock.
    pub async fn run<E, Fut>(
        &self,
        id: &str,
        who: &str,
        when: &str,
        step: impl FnOnce() -> Fut,
    ) -> Result<StepOutcome, LedgerError<E>>
    where
        Fut: Future<Output = Result<(), E>>,
    {
        if id.is_empty() || id.contains('/') {
            return Err(LedgerError::BadId);
        }
        let (claim, done) = (self.key(id, "claim"), self.key(id, "done"));
        let store = LedgerError::Store;
        if self.backend.read(&done).await.map_err(store)?.is_some() {
            return Ok(StepOutcome::AlreadyDone);
        }
        let text = format!("{when} {who}");
        let won = self
            .backend
            .create_exclusive(&claim, text.clone().into_bytes())
            .await
            .map_err(LedgerError::Store)?;
        if !won {
            // Lost the claim. If the winner has finished meanwhile this is a done step, else it is held.
            if self
                .backend
                .read(&done)
                .await
                .map_err(LedgerError::Store)?
                .is_some()
            {
                return Ok(StepOutcome::AlreadyDone);
            }
            let held = self
                .backend
                .read(&claim)
                .await
                .map_err(LedgerError::Store)?
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_else(|| "(claim vanished)".to_string());
            return Ok(StepOutcome::Claimed(held));
        }
        match step().await {
            Ok(()) => {
                self.backend
                    .write(&done, text.into_bytes())
                    .await
                    .map_err(LedgerError::Store)?;
                Ok(StepOutcome::Ran)
            }
            Err(e) => {
                // Only the holder of the claim reaches here, so removing it cannot race a taker.
                self.backend
                    .delete(&claim)
                    .await
                    .map_err(LedgerError::Store)?;
                Err(LedgerError::Step(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlob;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn counted(runs: &AtomicUsize) -> Result<(), ()> {
        runs.fetch_add(1, Ordering::SeqCst);
        // A suspension point inside the step, so a missing claim lets another migrator in.
        tokio::task::yield_now().await;
        Ok(())
    }

    #[tokio::test]
    async fn four_migrators_racing_one_step_run_it_once() {
        let b = MemoryBlob::new();
        let l = Ledger::new(&b, "migrations/");
        let runs = AtomicUsize::new(0);
        let one = || async { l.run("0001-layout", "a", "t0", || counted(&runs)).await };
        let (r1, r2, r3, r4) = tokio::join!(one(), one(), one(), one());
        let ran = [r1, r2, r3, r4]
            .into_iter()
            .filter(|r| matches!(r, Ok(StepOutcome::Ran)))
            .count();
        assert_eq!((ran, runs.load(Ordering::SeqCst)), (1, 1));
    }

    /// The known-bad control: the same race with the claim left out (check the done marker, run, write
    /// it) runs the step more than once, so the test above can tell a ledger from no ledger.
    #[tokio::test]
    async fn without_the_claim_the_same_race_runs_it_twice_or_more() {
        let b = MemoryBlob::new();
        let runs = AtomicUsize::new(0);
        let naive = || async {
            if b.read("m/0001.done").await.unwrap().is_none() {
                counted(&runs).await.unwrap();
                b.write("m/0001.done", vec![1]).await.unwrap();
            }
        };
        tokio::join!(naive(), naive(), naive(), naive());
        assert!(runs.load(Ordering::SeqCst) > 1);
    }

    #[tokio::test]
    async fn a_finished_step_is_not_run_again() {
        let b = MemoryBlob::new();
        let l = Ledger::new(&b, "migrations/");
        let runs = AtomicUsize::new(0);
        let first = l.run("s1", "a", "t0", || counted(&runs)).await.unwrap();
        let again = l.run("s1", "b", "t1", || counted(&runs)).await.unwrap();
        assert_eq!((first, again), (StepOutcome::Ran, StepOutcome::AlreadyDone));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    /// The state a killed migrator leaves (claim, no done marker), made directly: the next run reports
    /// the claim by name and does not run the step. A real killed-process test is not built.
    #[tokio::test]
    async fn a_claim_without_done_is_reported_by_name_not_rerun() {
        let b = MemoryBlob::new();
        b.write("migrations/s1.claim", b"t0 host1:77".to_vec())
            .await
            .unwrap();
        let l = Ledger::new(&b, "migrations/");
        let runs = AtomicUsize::new(0);
        let r = l.run("s1", "b", "t1", || counted(&runs)).await.unwrap();
        assert_eq!(r, StepOutcome::Claimed("t0 host1:77".to_string()));
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_failed_step_releases_its_claim_and_can_run_again() {
        let b = MemoryBlob::new();
        let l = Ledger::new(&b, "migrations/");
        let bad = l
            .run("s1", "a", "t0", || async { Err::<(), _>("boom") })
            .await;
        assert!(matches!(bad, Err(LedgerError::Step("boom"))));
        let runs = AtomicUsize::new(0);
        let ok = l.run("s1", "a", "t1", || counted(&runs)).await.unwrap();
        assert_eq!(ok, StepOutcome::Ran);
    }

    #[tokio::test]
    async fn a_bad_id_is_refused() {
        let b = MemoryBlob::new();
        let l = Ledger::new(&b, "migrations/");
        let r = l.run("a/b", "a", "t", || async { Ok::<(), ()>(()) }).await;
        assert!(matches!(r, Err(LedgerError::BadId)));
    }
}
