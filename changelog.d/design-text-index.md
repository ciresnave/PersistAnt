kind: docs: design note for a text index (ROADMAP row 39)
---
`docs/DESIGN-TEXT-INDEX.md`: proposes a lexical-search layer over kv (leaf `text.index`, same contract on every backend with `kv.batch_atomic` and `kv.ordered_scan_values`) first and native engines later as a separate capability; states what v1 promises (tokenisation, AND and prefix queries, term-frequency ordering, deletes that remove postings, erased delete through `kv.delete_erased`), the key layout, the tests with their controls, what it does not do, and four open questions for the PM and SageAnt. Documentation only; no code.
