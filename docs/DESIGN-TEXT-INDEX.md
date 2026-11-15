# Design: a text index (lexical search), ROADMAP row 39

Status: **proposal, for the PM and SageAnt to answer; no code until it is agreed.** Written 2026-11-15 by the
PersistAnt lane. Request: SageAnt (Knowledge Store), via the PM, 2026-11-14. Today SageAnt keeps its postings in
a `kv-postings` store through its own `KvTextIndex` over a kv backend; nothing in PersistAnt offers lexical search
(the only mentions of full text are `tsvector` decoded loudly, `docs/DECODER-AUDIT.md`).

## 1. What is asked, and the two questions under it

1. **Does a text index belong in this project at all?** It does if it keeps PersistAnt's one rule: *a capability
   is declared only where a test passed, and the same id means the same thing on every backend that declares it.*
2. **What does it promise?** Tokenisation, what a query can say, ordering of results, and what a delete does to
   the postings (this ties to row 38, `kv.delete_erased`: SageAnt wants erase-on-forget to reach the index too).

## 2. Two ways to build it

| | **A. A layer over kv** (like `Indexed`, `docs/DESIGN-AUTH-STORAGE.md` 4.3) | **B. Native** (`fts5`, `tsvector`, MySQL `FULLTEXT`) |
|---|---|---|
| Backends | every backend with `kv.batch_atomic` + `kv.ordered_scan_values`: memory, SQLite, PostgreSQL, MySQL, MariaDB; refused on a blob store, as `Indexed` is | one implementation per engine; none for memory or a blob store |
| Same answer everywhere | **yes**: one tokeniser, one matcher, in this crate | **no**: each engine tokenises, folds case and ranks its own way; a result list differs by backend |
| Speed | a prefix scan per term plus a join in Rust | engine-native, much faster on large corpora |
| Deletes and erase | postings are ordinary keys: `kv.delete_erased` reaches them | fts5 keeps shadow tables (`secure_delete` covers them, untested); PostgreSQL keeps dead tuples until vacuum |
| Ranking | what we write | BM25 and friends, built in |

**Recommendation: A for the first release, B later and only as an explicit second capability** (for example
`text.native`, one row per engine, never claimed to give A's results). A is the only option that keeps the rule in
section 1 and works on SageAnt's embedded backend (SQLite) and on the in-memory test fake.

## 3. What A would promise (version 1)

New layer `persistant_kv::text::TextIndex<'a, K>`, leaf `text.index`, over a borrowed `KvBackend`:

- **Tokenisation.** Unicode word boundaries (UAX #29), lower-cased after NFKC folding; tokens of 1 or 2 characters
  kept, tokens over 64 bytes dropped. One implementation, tested on a fixed corpus (ASCII, accents, CJK as single
  characters, apostrophes, digits). **No stemming, no stop words, no language detection**: a consumer that wants
  them tokenises before calling (the index also accepts pre-tokenised input).
- **Documents.** `put(index, doc_id, text)` stores the document's postings and a *document record* listing its
  terms, in **one `batch`** (so a crash never leaves postings without the record or the reverse). `put` again
  replaces: old postings not in the new term set are removed in the same batch. `ttl` follows the kv rule (all keys
  of a document expire together).
- **Queries.** `search(index, query, limit, after)`: every term must match (AND); a trailing `*` makes a term a
  prefix; a quoted phrase is **not** in v1. Results come from one snapshot (`ordered_scan_values`).
- **Order.** v1 returns documents ordered by the sum of term frequencies, ties by `doc_id` ascending. **No BM25**:
  it needs corpus statistics (document count, average length) that every write would update, a hot key that
  serialises all writers. If SageAnt measures that term-frequency ordering is not enough, BM25 with statistics
  kept per shard of the id space (approximate, stated as such) is the proposed next step.
- **Delete.** `remove(index, doc_id)` deletes the postings and the document record in one batch. `remove_erased`
  (only where the backend declares `kv.delete_erased`) erases each posting key then the document record, **record
  last**: the record is the journal of what is left to erase, so a crash mid-way is finished by calling it again.
  It is a sequence, not a batch, and its cost on SQLite (one truncating checkpoint per key, `execute_erased`) is
  linear in the number of distinct terms: if that is too slow the answer is a bulk `delete_erased_many`, a
  follow-up in row 38's family.

## 4. Key layout

Keys are strings, so the separator is `U+001F` (never produced by the tokeniser):

| Key | Value | For |
|---|---|---|
| `tx␟{index}␟{term}␟{doc_id}` | term frequency (u32, big endian) | one posting; scanning `tx␟{index}␟{term}␟` is "documents with this term", scanning `tx␟{index}␟{prefix}` is a prefix query |
| `td␟{index}␟{doc_id}` | the document's terms with their frequencies | `put` (diff against the old set), `remove`, `remove_erased` |

A prefix query on a very common prefix scans many postings: the limit and `after` cursor page it, and a
`max_postings_scanned` guard returns an error rather than an unbounded scan.

## 5. How it would be tested (the repository's method)

`text.index` is certified like the other kv leaves, with a known-bad control for each check: (a) the fixed-corpus
tokeniser test; (b) AND and prefix queries against a brute-force scan of the same corpus (the oracle is a plain
`str::contains` over the tokens, a different route from the postings); (c) `put` over an existing document leaves
no stale posting (scan `tx␟` and compare to the new term set); (d) `remove` leaves **no key** under either prefix
for that document; (e) concurrent `put` of the same `doc_id` leaves postings of exactly one of the two texts
(sampled concurrency, trials in the row); (f) where `kv.delete_erased` is declared, `text.index.erased`: after
`remove_erased` a canary term is in none of the backend's files (the same raw-file read-back as `kv.delete_erased`,
plain `remove` as the control). Each defect is mutation-checked (drop the old-posting cleanup; fail the tests).

## 6. What it does not do (v1)

Stemming and synonyms; phrase and proximity queries; fuzzy matching; snippets and highlighting; BM25;
multi-language analysis; an index over a blob store (refused, as `Indexed` is); search across indexes in one call.

## 7. Questions that block the build

1. **PM:** is layer-first (A) agreed, with native engines left for a later, separately named capability?
2. **SageAnt:** please send your `KvTextIndex` key layout and the queries it runs today (AND only? prefix? phrase?
   how are results ordered, and how big is the corpus: documents and distinct terms). If a v1 that matches your
   layout lets you delete your code, we should shape the keys to it before writing ours.
3. **SageAnt:** is term-frequency ordering acceptable, or is BM25 a requirement for v1?
4. **PM / CireSnave:** the tokeniser needs Unicode segmentation and normalisation crates (new dependencies, at
   their latest versions, per the standing rule). Any objection to adding them to `persistant-kv`, or should the
   tokeniser sit behind a feature so the kv crate stays lean for consumers that do not index text?

## 8. Order of work once agreed

1. `persistant_kv::text` with the tokeniser and `put` / `remove` / `search`, leaf `text.index`, the tests above,
   matrix rows for memory, SQLite, `sqlite-full`, PostgreSQL, MySQL, MariaDB; refused on blob.
2. `remove_erased` and the `text.index.erased` leaf, behind `kv.delete_erased`.
3. Only if measured and asked: BM25 with sharded statistics; a native `text.native` capability per engine.
