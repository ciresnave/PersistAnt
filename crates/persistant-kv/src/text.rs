// SPDX-License-Identifier: MIT OR Apache-2.0
//! A text index as a layer over any [`KvBackend`] (`docs/DESIGN-TEXT-INDEX.md`), behind the cargo
//! feature `text-index` (no new dependency).
//!
//! The index takes **terms with their frequencies**, not text: it imposes no tokeniser, and a
//! consumer that has one keeps it. A *document* is an opaque `doc_id` the caller chooses; one index
//! name is one namespace. It hands out **integers**, so a consumer can rank bit-identically with its
//! own formula: [`TextIndex::postings`] (`doc_id` and term frequency, ascending by `doc_id`),
//! [`TextIndex::doc_len`], [`TextIndex::stats`] (documents and total length) and
//! [`TextIndex::docs`] (every document id). [`TextIndex::search`] is a convenience: any or all of
//! some terms, ordered by the sum of term frequencies, ties by `doc_id` ascending. No BM25.
//!
//! [`TextIndex::remove_many_erased`] removes documents with their postings erased in the same batch
//! (`BatchOp::DeleteErased`; SQLite).
//!
//! Every change is **one [`KvBackend::batch`]** with a [`BatchOp::Require`] on the corpus totals
//! and on the document's own record, so a crash or a concurrent change never leaves postings
//! without their record, or totals that disagree with the documents. A lost race retries, at most
//! 64 times (yielding between tries). **The corpus totals are one record that every writer requires, so writers
//! serialise**; that is stated, not hidden, and is fine for personal stores (thousands to about
//! 100k documents, unmeasured above).
//!
//! The layer exists for a backend that has `kv.batch_atomic` and `kv.ordered_scan_values` (memory
//! and the SQL engines) and returns [`KvError::Unsupported`] on one that has not (a blob store).
//!
//! Keys (the separator is `U+001F`, which no index, document id or term may contain):
//!
//! | key | value |
//! |---|---|
//! | `tx␟{index}␟{term}␟{doc_id}` | term frequency, `u32` big endian |
//! | `td␟{index}␟{doc_id}` | length `u32`, term count `u32`, then each term: frequency `u32`, byte length `u32`, bytes |
//! | `tt␟{index}` | documents `u64`, total length `u64`, big endian |
//!
//! A document's **length** is the sum of its term frequencies.

use crate::{BatchOp, KvBackend, KvError};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

/// The separator inside keys.
const SEP: char = '\u{1f}';
/// A lost race is retried this many times, then reported. Each lost race means another writer
/// won, so this many writers racing at once all finish; more than that may report the race.
const MAX_TRIES: usize = 64;
/// Entries per page when the layer scans on its own account.
const PAGE: usize = 500;
/// The longest term, in bytes.
const MAX_TERM_BYTES: usize = 512;

/// Corpus totals of one index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct Stats {
    /// The number of documents.
    pub docs: u64,
    /// The sum of the documents' lengths (tokens).
    pub total_len: u64,
}

/// One page of the postings of a term.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PostingPage {
    /// `(doc_id, term frequency)`, ascending by `doc_id`.
    pub entries: Vec<(String, u32)>,
    /// `Some(doc_id)`: pass it as `after` for the next page. `None`: the postings are complete.
    pub next: Option<String>,
}

/// One page of document ids.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DocPage {
    /// Document ids, ascending.
    pub ids: Vec<String>,
    /// `Some(doc_id)`: pass it as `after` for the next page. `None`: the listing is complete.
    pub next: Option<String>,
}

/// How [`TextIndex::search`] combines its terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// A document matches if it holds **any** of the terms (a bag of words).
    Any,
    /// A document matches only if it holds **all** of the terms.
    All,
}

/// One result of [`TextIndex::search`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hit {
    /// The document.
    pub doc_id: String,
    /// The sum of its term frequencies over the matched terms.
    pub score: u64,
}

/// A text index on a borrowed [`KvBackend`].
#[derive(Debug)]
pub struct TextIndex<'a, K> {
    kv: &'a K,
}

impl<K> Clone for TextIndex<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for TextIndex<'_, K> {}

fn invalid(what: &str, why: &str) -> KvError {
    KvError::Backend(format!("text index: {what} {why}"))
}

fn corrupt(what: &str) -> KvError {
    KvError::Corrupt(format!("text index: {what}"))
}

fn check_name(what: &str, s: &str) -> Result<(), KvError> {
    if s.is_empty() {
        return Err(invalid(what, "is empty"));
    }
    if s.contains(SEP) {
        return Err(invalid(what, "contains U+001F"));
    }
    Ok(())
}

fn check_term(s: &str) -> Result<(), KvError> {
    check_name("a term", s)?;
    if s.len() > MAX_TERM_BYTES {
        return Err(invalid("a term", "is longer than 512 bytes"));
    }
    Ok(())
}

fn posting_prefix(index: &str, term: &str) -> String {
    format!("tx{SEP}{index}{SEP}{term}{SEP}")
}

fn posting_key(index: &str, term: &str, doc_id: &str) -> String {
    format!("{}{doc_id}", posting_prefix(index, term))
}

fn doc_prefix(index: &str) -> String {
    format!("td{SEP}{index}{SEP}")
}

fn doc_key(index: &str, doc_id: &str) -> String {
    format!("{}{doc_id}", doc_prefix(index))
}

fn totals_key(index: &str) -> String {
    format!("tt{SEP}{index}")
}

fn encode_totals(s: Stats) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend_from_slice(&s.docs.to_be_bytes());
    out.extend_from_slice(&s.total_len.to_be_bytes());
    out
}

fn decode_totals(bytes: Option<&[u8]>) -> Result<Stats, KvError> {
    let Some(b) = bytes else {
        return Ok(Stats::default());
    };
    let b: [u8; 16] = b.try_into().map_err(|_| corrupt("the totals record"))?;
    Ok(Stats {
        docs: u64::from_be_bytes(b[..8].try_into().expect("8 bytes")),
        total_len: u64::from_be_bytes(b[8..].try_into().expect("8 bytes")),
    })
}

fn encode_doc(len: u32, terms: &BTreeMap<String, u32>) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&(terms.len() as u32).to_be_bytes());
    for (t, tf) in terms {
        out.extend_from_slice(&tf.to_be_bytes());
        out.extend_from_slice(&(t.len() as u32).to_be_bytes());
        out.extend_from_slice(t.as_bytes());
    }
    out
}

fn decode_doc(bytes: &[u8]) -> Result<(u32, BTreeMap<String, u32>), KvError> {
    fn u32_at(b: &[u8], at: &mut usize) -> Result<u32, KvError> {
        let end = at.checked_add(4).filter(|e| *e <= b.len());
        let end = end.ok_or_else(|| corrupt("a document record is cut short"))?;
        let v = u32::from_be_bytes(b[*at..end].try_into().expect("4 bytes"));
        *at = end;
        Ok(v)
    }
    let mut at = 0;
    let len = u32_at(bytes, &mut at)?;
    let n = u32_at(bytes, &mut at)?;
    let mut terms = BTreeMap::new();
    for _ in 0..n {
        let tf = u32_at(bytes, &mut at)?;
        let tl = u32_at(bytes, &mut at)? as usize;
        let end = at.checked_add(tl).filter(|e| *e <= bytes.len());
        let end = end.ok_or_else(|| corrupt("a document record is cut short"))?;
        let t = std::str::from_utf8(&bytes[at..end])
            .map_err(|_| corrupt("a term is not UTF-8"))?
            .to_string();
        at = end;
        terms.insert(t, tf);
    }
    if at != bytes.len() {
        return Err(corrupt("a document record has trailing bytes"));
    }
    Ok((len, terms))
}

/// Merge the caller's `(term, frequency)` pairs: equal terms add up, a zero frequency is refused.
fn merge_terms(terms: &[(String, u32)]) -> Result<(BTreeMap<String, u32>, u32), KvError> {
    let mut merged: BTreeMap<String, u32> = BTreeMap::new();
    let mut len: u64 = 0;
    for (t, tf) in terms {
        check_term(t)?;
        if *tf == 0 {
            return Err(invalid("a term frequency", "is zero"));
        }
        let e = merged.entry(t.clone()).or_insert(0);
        *e = e
            .checked_add(*tf)
            .ok_or_else(|| invalid("a term frequency", "overflows u32"))?;
        len += u64::from(*tf);
    }
    let len = u32::try_from(len).map_err(|_| invalid("a document length", "overflows u32"))?;
    Ok((merged, len))
}

impl<'a, K: KvBackend> TextIndex<'a, K> {
    /// Index documents on `kv`.
    pub fn new(kv: &'a K) -> Self {
        Self { kv }
    }

    /// The backend underneath.
    pub fn kv(&self) -> &'a K {
        self.kv
    }

    /// Store the document `doc_id` of `index` with its `terms` and their frequencies, replacing an
    /// earlier version of it: the postings no longer in the new set are removed, and the corpus
    /// totals move by the difference, all in one batch. Equal terms add up; a term frequency of
    /// zero, an empty name, or a `U+001F` in a name is refused.
    pub async fn put(
        &self,
        index: &str,
        doc_id: &str,
        terms: &[(String, u32)],
    ) -> Result<(), KvError> {
        check_name("the index name", index)?;
        check_name("a document id", doc_id)?;
        let (new, new_len) = merge_terms(terms)?;
        let (tt_key, td_key) = (totals_key(index), doc_key(index, doc_id));
        for _ in 0..MAX_TRIES {
            let tt_old = self.kv.get(&tt_key).await?;
            let td_old = self.kv.get(&td_key).await?;
            let totals = decode_totals(tt_old.as_deref())?;
            let (old_len, old_terms) = match &td_old {
                Some(b) => decode_doc(b)?,
                None => (0, BTreeMap::new()),
            };
            let docs = if td_old.is_some() {
                totals.docs
            } else {
                totals.docs + 1
            };
            let total_len = totals
                .total_len
                .checked_sub(u64::from(old_len))
                .ok_or_else(|| corrupt("the totals are below a document's length"))?
                + u64::from(new_len);
            let mut ops = vec![
                BatchOp::require(tt_key.clone(), tt_old.clone()),
                BatchOp::require(td_key.clone(), td_old.clone()),
            ];
            for (t, tf) in &new {
                ops.push(BatchOp::put(
                    posting_key(index, t, doc_id),
                    tf.to_be_bytes().to_vec(),
                ));
            }
            for t in old_terms.keys().filter(|t| !new.contains_key(*t)) {
                ops.push(BatchOp::delete(posting_key(index, t, doc_id)));
            }
            ops.push(BatchOp::put(td_key.clone(), encode_doc(new_len, &new)));
            ops.push(BatchOp::put(
                tt_key.clone(),
                encode_totals(Stats { docs, total_len }),
            ));
            if self.kv.batch(&ops).await? {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(KvError::Backend(format!(
            "text index: put of {doc_id:?} lost the race {MAX_TRIES} times"
        )))
    }

    /// Remove the documents `doc_ids` of `index`, with every posting of theirs, and take them out
    /// of the corpus totals, in one batch. Absent documents are ignored; the result is how many
    /// were removed.
    pub async fn remove_many(&self, index: &str, doc_ids: &[&str]) -> Result<usize, KvError> {
        self.remove_inner(index, doc_ids, false).await
    }

    /// As [`remove_many`](Self::remove_many), and the postings (they hold the documents' terms) and
    /// the document records are removed with [`BatchOp::DeleteErased`] in the **same atomic batch**:
    /// where the backend can (SQLite), the stored bytes are overwritten as part of it and the
    /// write-ahead log is truncated after the commit. A backend that cannot returns
    /// [`KvError::Unsupported`] before anything changes (`kv.delete_erased` inside a batch is
    /// required); the corpus totals are counts, not content, and are written plainly.
    ///
    /// **If it returns an error after the batch committed** (the log could not be truncated because a
    /// reader held it), the documents are gone from the index but their terms may still be in the log:
    /// call it again with the same ids. Documents already removed are ignored, and a repeat call
    /// still runs an erased step, which is what truncates the log. **Not covered:** a posting or
    /// record an earlier plain `put` replaced or a plain removal deleted (see `kv.delete_erased` and
    /// the `sqlite-secure` profile), stale in-page fragments of earlier writes (on `sqlite` and
    /// `sqlite-full`, with many documents, some of a forgotten term can stay in the file; only
    /// `sqlite-secure` leaves none), and copies a file system, snapshot or backup keeps.
    pub async fn remove_many_erased(
        &self,
        index: &str,
        doc_ids: &[&str],
    ) -> Result<usize, KvError> {
        self.remove_inner(index, doc_ids, true).await
    }

    async fn remove_inner(
        &self,
        index: &str,
        doc_ids: &[&str],
        erased: bool,
    ) -> Result<usize, KvError> {
        let delete = |key: String| {
            if erased {
                BatchOp::delete_erased(key)
            } else {
                BatchOp::delete(key)
            }
        };
        check_name("the index name", index)?;
        let wanted: BTreeSet<&str> = doc_ids.iter().copied().collect();
        for id in &wanted {
            check_name("a document id", id)?;
        }
        let tt_key = totals_key(index);
        for _ in 0..MAX_TRIES {
            let tt_old = self.kv.get(&tt_key).await?;
            let totals = decode_totals(tt_old.as_deref())?;
            let mut ops = vec![BatchOp::require(tt_key.clone(), tt_old.clone())];
            let (mut removed, mut freed) = (0_u64, 0_u64);
            for id in &wanted {
                let td_key = doc_key(index, id);
                let td_old = self.kv.get(&td_key).await?;
                ops.push(BatchOp::require(td_key.clone(), td_old.clone()));
                let Some(bytes) = td_old else { continue };
                let (len, terms) = decode_doc(&bytes)?;
                for t in terms.keys() {
                    ops.push(delete(posting_key(index, t, id)));
                }
                ops.push(delete(td_key));
                removed += 1;
                freed += u64::from(len);
            }
            if removed == 0 {
                if erased {
                    // Nothing to remove, but a repeat after a log that could not be truncated must
                    // still run an erased step: a batch with an erased delete of a key that is not
                    // there (the index's marker name, never written) does that and changes nothing.
                    let marker = format!("tm{SEP}{index}");
                    let _ = self
                        .kv
                        .batch(&[
                            BatchOp::require(tt_key.clone(), tt_old.clone()),
                            BatchOp::delete_erased(marker),
                        ])
                        .await?;
                }
                return Ok(0);
            }
            let docs = totals
                .docs
                .checked_sub(removed)
                .ok_or_else(|| corrupt("the totals count fewer documents than were removed"))?;
            let total_len = totals
                .total_len
                .checked_sub(freed)
                .ok_or_else(|| corrupt("the totals are below the removed lengths"))?;
            // An index with no documents holds no keys at all.
            ops.push(if docs == 0 {
                BatchOp::delete(tt_key.clone())
            } else {
                BatchOp::put(tt_key.clone(), encode_totals(Stats { docs, total_len }))
            });
            if self.kv.batch(&ops).await? {
                return Ok(removed as usize);
            }
            tokio::task::yield_now().await;
        }
        Err(KvError::Backend(format!(
            "text index: remove lost the race {MAX_TRIES} times"
        )))
    }

    /// Up to `limit` postings of `term`, `(doc_id, term frequency)`, ascending by `doc_id` and
    /// strictly after `after`, from one snapshot. The number of documents holding a term is the
    /// number of entries over all pages.
    pub async fn postings(
        &self,
        index: &str,
        term: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<PostingPage, KvError> {
        check_name("the index name", index)?;
        check_term(term)?;
        let prefix = posting_prefix(index, term);
        let after_key = after.map(|d| format!("{prefix}{d}"));
        let page = self
            .kv
            .ordered_scan_values(&prefix, after_key.as_deref(), limit)
            .await?;
        let strip = |k: &str| -> Result<String, KvError> {
            k.strip_prefix(&prefix)
                .map(str::to_string)
                .ok_or_else(|| corrupt("a posting key outside its prefix"))
        };
        let mut entries = Vec::with_capacity(page.entries.len());
        for (k, v) in &page.entries {
            let tf: [u8; 4] = v.as_slice().try_into().map_err(|_| corrupt("a posting"))?;
            entries.push((strip(k)?, u32::from_be_bytes(tf)));
        }
        let next = page.next.as_deref().map(strip).transpose()?;
        Ok(PostingPage { entries, next })
    }

    /// The length (sum of term frequencies) of `doc_id`, or `None` if it is not in the index.
    pub async fn doc_len(&self, index: &str, doc_id: &str) -> Result<Option<u32>, KvError> {
        check_name("the index name", index)?;
        check_name("a document id", doc_id)?;
        match self.kv.get(&doc_key(index, doc_id)).await? {
            Some(b) => Ok(Some(decode_doc(&b)?.0)),
            None => Ok(None),
        }
    }

    /// The corpus totals of `index`: documents and total length. An index nobody wrote to is zero.
    pub async fn stats(&self, index: &str) -> Result<Stats, KvError> {
        check_name("the index name", index)?;
        decode_totals(self.kv.get(&totals_key(index)).await?.as_deref())
    }

    /// Up to `limit` document ids of `index`, ascending and strictly after `after`: the
    /// "all documents" scan (to rebuild, or to find a document whose owner has forgotten it).
    pub async fn docs(
        &self,
        index: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<DocPage, KvError> {
        check_name("the index name", index)?;
        let prefix = doc_prefix(index);
        let after_key = after.map(|d| format!("{prefix}{d}"));
        let page = self
            .kv
            .ordered_scan(&prefix, after_key.as_deref(), limit)
            .await?;
        let strip = |k: &str| -> Result<String, KvError> {
            k.strip_prefix(&prefix)
                .map(str::to_string)
                .ok_or_else(|| corrupt("a document key outside its prefix"))
        };
        let ids = page
            .keys
            .iter()
            .map(|k| strip(k))
            .collect::<Result<Vec<_>, _>>()?;
        let next = page.next.as_deref().map(strip).transpose()?;
        Ok(DocPage { ids, next })
    }

    /// Up to `limit` document ids of `index` that start with `id_prefix`, ascending and strictly
    /// after `after` (which must itself start with the prefix): the range scan under a prefix such
    /// as `"<id>/"`, without paging past the other documents. An empty prefix is [`docs`](Self::docs).
    pub async fn docs_with_prefix(
        &self,
        index: &str,
        id_prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<DocPage, KvError> {
        check_name("the index name", index)?;
        if id_prefix.contains(SEP) {
            return Err(invalid("a document id prefix", "must not contain U+001F"));
        }
        let strip_to = doc_prefix(index);
        let prefix = format!("{strip_to}{id_prefix}");
        let after_key = after.map(|d| format!("{strip_to}{d}"));
        let page = self
            .kv
            .ordered_scan(&prefix, after_key.as_deref(), limit)
            .await?;
        let strip = |k: &str| -> Result<String, KvError> {
            k.strip_prefix(&strip_to)
                .map(str::to_string)
                .ok_or_else(|| corrupt("a document key outside its prefix"))
        };
        let ids = page
            .keys
            .iter()
            .map(|k| strip(k))
            .collect::<Result<Vec<_>, _>>()?;
        let next = page.next.as_deref().map(strip).transpose()?;
        Ok(DocPage { ids, next })
    }

    /// Remove, as [`remove_many_erased`](Self::remove_many_erased) does, every document of `index`
    /// whose id starts with `id_prefix` (an empty prefix removes the whole index); the result is how
    /// many were removed.
    ///
    /// It works in rounds of up to 100 documents, **one atomic erased batch per round**: a round
    /// either removes all of its documents with their postings or none. **A crash, or an error,
    /// between rounds leaves the earlier rounds removed and the rest present** (a partial removal,
    /// never a half-removed document); run it again with the same prefix and it carries on, which is
    /// also what truncates the log after an error. Documents written under the prefix while it runs
    /// may or may not be removed. The caveats of `remove_many_erased` apply to each round.
    pub async fn remove_prefix_erased(
        &self,
        index: &str,
        id_prefix: &str,
    ) -> Result<usize, KvError> {
        const ROUND: usize = 100;
        let mut total = 0;
        loop {
            let page = self.docs_with_prefix(index, id_prefix, None, ROUND).await?;
            if page.ids.is_empty() {
                if total == 0 {
                    // Nothing there: still run the erased step, so a repeat after a log that could
                    // not be truncated truncates it.
                    self.remove_inner(index, &[], true).await?;
                }
                return Ok(total);
            }
            let ids: Vec<&str> = page.ids.iter().map(String::as_str).collect();
            total += self.remove_inner(index, &ids, true).await?;
        }
    }

    /// Documents matching `terms` ([`Match::Any`] or [`Match::All`]), best first: by the sum of
    /// their term frequencies over the matched terms, ties by `doc_id` ascending, at most `limit`.
    /// A convenience, not a ranking: it does not use document length or term rarity (read
    /// [`postings`](Self::postings), [`doc_len`](Self::doc_len) and [`stats`](Self::stats) and
    /// rank yourself for that). Each term's postings come from a snapshot of their own; a change
    /// between two terms' reads may be seen for one and not the other.
    pub async fn search(
        &self,
        index: &str,
        terms: &[String],
        mode: Match,
        limit: usize,
    ) -> Result<Vec<Hit>, KvError> {
        check_name("the index name", index)?;
        let wanted: BTreeSet<&str> = terms.iter().map(String::as_str).collect();
        for t in &wanted {
            check_term(t)?;
        }
        if wanted.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut scores: BTreeMap<String, (usize, u64)> = BTreeMap::new();
        for term in &wanted {
            let mut after: Option<String> = None;
            loop {
                let page = self.postings(index, term, after.as_deref(), PAGE).await?;
                for (doc, tf) in page.entries {
                    let e = scores.entry(doc).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += u64::from(tf);
                }
                match page.next {
                    Some(c) => after = Some(c),
                    None => break,
                }
            }
        }
        let mut hits: Vec<Hit> = scores
            .into_iter()
            .filter(|(_, (n, _))| mode == Match::Any || *n == wanted.len())
            .map(|(doc_id, (_, score))| Hit { doc_id, score })
            .collect();
        hits.sort_by(|a, b| (Reverse(a.score), &a.doc_id).cmp(&(Reverse(b.score), &b.doc_id)));
        hits.truncate(limit);
        Ok(hits)
    }
}
