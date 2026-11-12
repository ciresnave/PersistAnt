kind: docs: design note for a single-connection Postgres session
---
Adds `docs/DESIGN-PG-NATIVE-SESSION.md`: whether and how `persistant-sqlx` offers one dedicated Postgres connection with
session state, read-only snapshot transactions, safe identifier quoting and COPY streaming, and the raw-`PgConnection`
alternative. No code change.
