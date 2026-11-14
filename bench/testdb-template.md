# TestDb template databases (ROADMAP row 36c)

One run, 2026-11-14, a throwaway test (not committed) against PostgreSQL 16 in a local Docker container on
Windows, databases made one after another with `Cleanup::Defer`. Schema: 60 tables with one index each (a
stand-in; it is **not** Humboldt's schema, which was not measured here).

| 19 databases | time |
|---|---|
| `create_with` + `migrate` each | 36.75 s (1.93 s each) |
| `template` once, then `create_from` each | 2.24 s + 9.52 s = 11.76 s (0.50 s per copy) |

So the migrate is paid once (2.24 s) and a copy costs the `CREATE DATABASE .. TEMPLATE` file copy. A single run on one
machine with one schema size: the ratio will differ with the schema and the server's disk, and concurrent copies
were not timed (the concurrency test checks correctness: eight callers, one template, eight copies).
