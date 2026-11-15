# Design: a text index (lexical search), ROADMAP row 39

Status: **step 1 built (`persistant_kv::text`, feature `text-index`: `put`, `remove_many`, `postings`, `doc_len`, `stats`,
`docs`, `search`; tests against an oracle on memory and SQLite; the matrix leaf `text.index`, the tokenising helper and the
erased removal follow, section 8); layer over kv, first (PM 2026-11-15); section 3 accepted by SageAnt 2026-11-15 (v1 as written, erased replace
not needed); waiting on the PM for the feature split in section 7 before code.** Written 2026-11-15 by the PersistAnt lane. Request: SageAnt (Knowledge Store), via
the PM, 2026-11-14. SageAnt keeps its postings today in `KvTextIndex` (sageant-persistant 0.3.8, `src/index.rs`)
over PersistAnt kv 0.18 on SQLite; nothing in PersistAnt offers lexical search (the only mention of full text is
`tsvector` decoded loudly, `docs/DECODER-AUDIT.md`).

## 1. What SageAnt told us (2026-11-15, relayed as quoted facts, not as our measurements)

- **Layout today:** `x/<term>/<id>/<version>` -> term frequency; `d/<id>/<version>` -> document length in tokens;
  `f/<id>/<version>/<term>` -> empty (forward list); `m/ix` -> `"<docs> <total tokens>"`. Every change is **one
  atomic `kv.batch` with a `Require` on the totals key**; a lost race retries (at most 16).
- **Terms:** maximal runs of Unicode-alphanumeric characters, lower-cased; no stemming, no stop words.
- **Query shape:** one: bag of words, **any term matches (OR)**, scores summed per document. No phrase, prefix,
  fuzzy or per-collection restriction in the index; the store filters visibility afterwards and the index may lag it.
- **Ranking:** SageAnt ranks with its own integer BM25 (k1 1.2, b 0.75 in thousandths, scores bit-identical on every
  machine). It needs, per (term, document): **tf, document length, the number of documents holding the term, the
  number of documents, the total tokens**. A term-frequency order alone is *not* enough for it; it does not need the
  index to rank.
- **Forget:** required, and stronger than delete: for every version of a lineage, the document entry, all postings
  **and the forward list** go, with the totals adjusted; the postings hold the terms of the forgotten text, so they
  must take the same erase path as the content. Until row 38 is usable its wording is "unretrievable", never "erased".
- **Scale:** no production corpus and no measurement; the design target is personal stores, thousands to about
  100k chunks. Anything larger is a guess.
- **Keeps:** an atomic multi-key batch with preconditions; per-version documents; a fast "all documents" scan
  (it rebuilds the index from the store on demand).

## 2. Two ways to build it

| | **A. A layer over kv** (like `Indexed`, `docs/DESIGN-AUTH-STORAGE.md` 4.3) | **B. Native** (`fts5`, `tsvector`, MySQL `FULLTEXT`) |
|---|---|---|
| Backends | every backend with `kv.batch_atomic` + `kv.ordered_scan_values`: memory, SQLite, PostgreSQL, MySQL, MariaDB; refused on a blob store, as `Indexed` is | one implementation per engine; none for memory or a blob store |
| Same answer everywhere | **yes**: one matcher, in this crate | **no**: each engine tokenises, folds case and ranks its own way |
| Hands out tf, length, n, N | yes, they are the stored numbers | partly, differently per engine |
| Deletes and erase | postings are ordinary keys: `kv.delete_erased` reaches them | fts5 keeps shadow tables; PostgreSQL keeps dead tuples until vacuum |

**A first; B later and only as an explicit second capability** (for example `text.native`, one row per engine, never
claimed to give A's results). A works on SageAnt's SQLite and on the in-memory fake, and gives the numbers SageAnt
needs because it stores them.

## 3. What A would promise (version 1): numbers first, ranking optional

New layer `persistant_kv::text::TextIndex<'a, K>`, leaf `text.index`, over a borrowed `KvBackend`. A **document** is
an opaque `doc_id` string chosen by the caller (SageAnt would use `<chunk id>/<version>`); one index name is one
namespace.

- **Write.** `put(index, doc_id, terms)` takes the document's terms with their frequencies (`&[(String, u32)]`; a
  helper turns text into them). In **one `batch`** it writes the postings, the document record (length and the term
  list: the forward list), and the corpus totals, with a `Require` on the totals record; a lost race retries
  inside, bounded (SageAnt's loop, 16). Putting an existing `doc_id` replaces it: postings no longer in the new set
  are removed in the same batch and the totals move by the difference. `remove_many(index, &[doc_id])` is one
  batch too (by lineage: the caller lists the versions); an index left with no documents holds no keys. **v1 has no
  expiry**: the corpus totals cannot expire with a document, so a time to live would make them drift.
- **Read, as numbers.** `postings(index, term, after, limit) -> PostingPage` of `(doc_id, tf)` in ascending `doc_id`
  order, from one snapshot (`ordered_scan_values`), paged; `doc_len(index, doc_id)`; `stats(index) -> (docs, total_len)`;
  `docs(index, after, limit)` scans the document records (the "all documents" scan). The number of documents
  holding a term is the posting count over the pages. All integers, no floats, so a consumer can rank bit-identically.
- **Read, as a query (convenience).** `search(index, terms, Any | All, limit)`: `Any` is SageAnt's bag of words,
  `All` is AND; results ordered by the sum of tf, ties by `doc_id` ascending. Each term's postings are from a snapshot
  of their own (`postings` is one snapshot per call); `search` is not one snapshot across terms. **No BM25 in v1** (a named later row,
  per the PM); SageAnt keeps its own ranker over `postings` / `doc_len` / `stats`, so it can delete `KvTextIndex`
  and implement its `TextIndex` trait over this.
- **Tokenisation is optional and separate.** The index takes terms, not text, so it imposes no tokeniser on a
  consumer that has one (SageAnt's alphanumeric-runs rule would keep working byte for byte). The helper that
  tokenises text (Unicode word boundaries, NFKC folding then lower case) is the part that needs
  `unicode-segmentation` and `unicode-normalization`, behind the PM's feature.
- **Corpus totals are one hot record.** Every writer `Require`s it, so writers serialise (a lost race retries). This
  is what SageAnt does today and its targets are personal stores; it is stated in the leaf's note, with the trial
  counts, and not hidden. A sharded count is the answer only if a measurement asks for it.
- **Not in the index:** visibility, collections or kinds (the store filters after; if a per-collection partition in
  the postings scan is wanted, it is a v2 row and SageAnt's "first thing we would ask for").

### Forget (erased removal), exactly what it does and does not do

`remove_erased(index, &[doc_id])`, only where the backend declares `kv.delete_erased`:

1. One batch: `Require` the document record and that no tombstone exists, write a tombstone `tm` key (no content),
   and subtract the documents' lengths from the totals. After it the documents no longer count in `stats`.
2. For each posting key of each document (read from the forward list), `delete_erased`; then `delete_erased` the
   document record **last**. The record is the journal: a crash anywhere is finished by calling it again, and the
   tombstone stops a second subtraction.
3. Delete the tombstone (not content-derived, so a plain delete).

It is **atomic for the statistics and resumable, not atomic for the postings**: between 1 and 2 a posting can still be
read. SageAnt already treats the store as the source of truth for visibility, which covers that window; a consumer
that does not must filter the same way. **What it covers:** the posting keys (they contain the terms), the forward
list, the document record. There is **no terms dictionary, segment or merge copy** in this design (a term exists only
as part of a posting key), so there is nothing else to erase. **What it does not cover, stated now:** (a) the
operating-system and engine caveats of `kv.delete_erased` (journals, snapshots, backups, wear levelling); (b) on
SQLite, **a posting or record that was removed or overwritten earlier by a plain batch** (a re-index that dropped
a term, a replaced document record) leaves its old bytes in the database's free space unless `secure_delete` was on
at that moment; this is the open case of row 38. **SageAnt does not need an erased replace** (a version's text is immutable, so a
re-index rewrites the same terms and drops none; whole-document erased removal is the only forget), so v1 has none.
The closure for every other case is an opt-in `SqliteBackend` profile with `secure_delete` on for every connection,
a row-38 follow-up declared as a capability (section 8, item 4) that SageAnt names as a **preference** in its storage
need, never a requirement, so "unretrievable" becomes "erased" only on a store that has it.

## 4. Key layout

Keys are strings; the separator is `U+001F` (never produced by the tokeniser, and a caller's terms must not contain it):

| Key | Value | For |
|---|---|---|
| `tx␟{index}␟{term}␟{doc_id}` | tf (u32, big endian) | one posting; scanning `tx␟{index}␟{term}␟` is "documents with this term" |
| `td␟{index}␟{doc_id}` | length (u32), then the terms with tf | `put` (diff against the old set), `remove*`, `docs` |
| `tt␟{index}` | documents, total tokens (two u64) | `stats`; the `Require` target |
| `tm␟{index}␟{doc_id}` | empty | erased removal in progress |

## 5. How it would be tested (the repository's method)

`text.index` is certified like the other kv leaves, with a known-bad control for each check: (a) postings,
`doc_len` and `stats` agree with a brute-force count over the same corpus after every operation of a scripted
sequence of puts, replaces and removals (the oracle is a plain map in the test, a different route from the keys);
(b) `Any` and `All` against the same oracle; (c) a replace leaves no stale posting (scan `tx␟` and compare to the
new term set); (d) `remove_many` leaves no key for the document under any prefix; (e) concurrent puts of different
documents keep `stats` exact, and concurrent puts of the same `doc_id` leave the postings of exactly one text (sampled
concurrency, trials in the row); (f) where `kv.delete_erased` is declared, leaf `text.index.erased`: after
`remove_erased` a canary term is in none of the backend's files (the raw-file read-back of `kv.delete_erased`, with
plain `remove_many` as the known-bad control), a crash injected between steps 1 and 2 is finished by a second call
and `stats` is subtracted once. Each defect is mutation-checked (drop the old-posting cleanup, skip the tombstone,
skip the totals Require: the tests fail).

## 6. What it does not do (v1)

BM25; stemming and synonyms; phrase, prefix and fuzzy queries (prefix is the first addition, one more scan range);
snippets; multi-language analysis; an index over a blob store (refused, as `Indexed` is); search across indexes in
one call; a per-collection partition; a bound on corpus size beyond "personal stores" (unmeasured above ~100k documents).

## 7. Dependencies and features (PM ruling 2026-11-15)

`unicode-segmentation` and `unicode-normalization` from crates.io at their newest releases (never git or path),
behind a cargo feature of `persistant-kv`, so `persistant-kv` without it pulls neither; SageAnt enables it. At
writing (crates.io, 2026-11-15):

| Crate | Newest | Its MSRV (`rust_version`) | Licence |
|---|---|---|---|
| `unicode-segmentation` | 1.13.3 | 1.85.0 | MIT OR Apache-2.0 |
| `unicode-normalization` | 0.1.26 | 1.36 | (MIT OR Apache-2.0) AND Unicode-3.0 |

Both are below this workspace's 1.94, so neither raises it. `unicode-normalization` carries the Unicode-3.0
licence term, which `deny.toml` already allows (line 22, read at `origin/main`); the feature's PR still runs
`cargo deny` with the feature on. **One refinement to confirm:** because the index takes terms, not text, only the
tokenising helper needs the two crates. Proposed: feature `text-index` (the index; no new dependency) and feature
`text-index-tokenizer` (the helper; the two crates), so SageAnt, which has its own tokeniser, pulls neither. If the
PM prefers one feature, `text-index` pulls both.

## 8. Order of work once agreed

1. `persistant_kv::text`: `put`, `remove_many`, `postings`, `doc_len`, `stats`, `docs`, `search`; leaf `text.index`;
   the tests above; matrix rows for memory, SQLite, `sqlite-full`, PostgreSQL, MySQL, MariaDB; refused on blob.
2. The tokenising helper and its fixed-corpus test (feature `text-index-tokenizer`).
3. `remove_erased` and leaf `text.index.erased`, behind `kv.delete_erased`. `docs(index, after, limit)` returns the
   `doc_id`s of the document records, so a consumer can find a document whose record exists but whose own store
   entry is gone (SageAnt's retry-until-done for an interrupted removal).
4. Row-38 follow-up: an opt-in SQLite profile with `secure_delete` on for every connection (closes the "removed
   earlier by a plain batch" gap for every table, not only this index), declared as a capability a consumer can
   list as a preference (the registry's `preferred` list, `Listing::preferred`), tested by the same raw-file read-back
   with a plain `delete` and `put` overwrite as controls. SageAnt's request, 2026-11-15.
5. Only if measured and asked: prefix queries; BM25 with sharded statistics; a native `text.native` per engine.

## 9. Questions still open

1. **PM:** one feature or two (section 7)?
2. Answered by SageAnt 2026-11-15: pre-computed `(term, tf)` pairs suit its flow; erased replace is not needed.
