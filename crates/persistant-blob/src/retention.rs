// SPDX-License-Identifier: MIT OR Apache-2.0
//! Retention for keys that carry their time (ROADMAP row 19, `docs/DESIGN-NONSQL-HELPERS.md`).
//!
//! A blob store has no compare-and-delete, so a sweep that deletes what it saw to be "expired" can remove
//! a value written again after it looked. [`purge_prefix_older_than`] is therefore correct **only** under
//! one key discipline: **the key names its period (`events/2026-11-22/...`) and no writer ever writes a
//! key again once its period has passed.** Under that discipline a key older than the cutoff is never
//! being written, so deleting it cannot lose a fresh value. Without it, do not use this function.
//! The caller supplies the parse from key to period; a key it cannot parse is kept and counted.

use crate::{BlobBackend, BlobError};

/// What a sweep did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Purged {
    /// Keys deleted: their period was strictly before the cutoff.
    pub deleted: usize,
    /// Keys kept because their period was the cutoff or later.
    pub kept: usize,
    /// Keys kept because `period_of` could not read a period from them (never deleted).
    pub unparsed: usize,
}

/// Delete every key under `prefix` whose period (as `period_of` reads it from the key) is **strictly
/// before** `cutoff`. Keys at the cutoff or later, and keys `period_of` returns `None` for, are kept.
/// See the module documentation for the key discipline this depends on.
pub async fn purge_prefix_older_than<B, P>(
    backend: &B,
    prefix: &str,
    cutoff: &P,
    period_of: impl Fn(&str) -> Option<P>,
) -> Result<Purged, BlobError>
where
    B: BlobBackend,
    P: Ord,
{
    const PAGE: usize = 256;
    let mut out = Purged::default();
    let mut after: Option<String> = None;
    loop {
        let page = backend.list(prefix, after.as_deref(), PAGE).await?;
        for entry in &page.entries {
            match period_of(&entry.key) {
                None => out.unparsed += 1,
                Some(p) if p < *cutoff => {
                    backend.delete(&entry.key).await?;
                    out.deleted += 1;
                }
                Some(_) => out.kept += 1,
            }
        }
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryBlob;

    /// `events/<period>/<name>` with a four-digit period.
    fn period(key: &str) -> Option<u32> {
        key.strip_prefix("events/")?.get(..4)?.parse().ok()
    }

    async fn store() -> MemoryBlob {
        let b = MemoryBlob::new();
        for k in [
            "events/0001/a",
            "events/0002/a",
            "events/0003/a",
            "events/0004/a",
            "events/junk",
            "other/0001/a",
        ] {
            b.write(k, vec![1]).await.unwrap();
        }
        b
    }

    #[tokio::test]
    async fn older_periods_go_and_the_cutoff_and_later_stay() {
        let b = store().await;
        let r = purge_prefix_older_than(&b, "events/", &3, period)
            .await
            .unwrap();
        assert_eq!(
            r,
            Purged {
                deleted: 2,
                kept: 2,
                unparsed: 1
            }
        );
        // The known-bad control: the period AT the cutoff must survive (a `<=` would delete it).
        assert!(b.read("events/0003/a").await.unwrap().is_some());
        assert!(b.read("events/0004/a").await.unwrap().is_some());
        assert!(b.read("events/0001/a").await.unwrap().is_none());
        assert!(b.read("events/0002/a").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_unparsed_key_and_other_prefixes_are_never_touched() {
        let b = store().await;
        purge_prefix_older_than(&b, "events/", &u32::MAX, period)
            .await
            .unwrap();
        assert!(
            b.read("events/junk").await.unwrap().is_some(),
            "unparsed kept"
        );
        assert!(
            b.read("other/0001/a").await.unwrap().is_some(),
            "other prefix kept"
        );
        assert!(b.read("events/0004/a").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn it_pages_through_more_keys_than_one_page() {
        let b = MemoryBlob::new();
        for n in 0..600_u32 {
            b.write(&format!("events/{:04}/x", n % 2 * 5000 + n / 2), vec![1])
                .await
                .unwrap();
        }
        let r = purge_prefix_older_than(&b, "events/", &5000, period)
            .await
            .unwrap();
        assert_eq!(r.deleted + r.kept + r.unparsed, 600);
        assert!(r.deleted > 256, "more than a page was deleted: {r:?}");
    }
}
