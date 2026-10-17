# Row decoder audit: which column types a consumer can read

Scope: `RelationalBackend::query` in `persistant-sqlx` (PostgreSQL, MySQL, MariaDB, SQLite) returns
`Value` (`Null`, `Bool`, `Int`, `Float`, `Text`, `Bytes`). This is what each column type becomes.
Measured against PostgreSQL 16, MySQL 8.4, MariaDB 11.4 and the bundled SQLite, at 0.7.12 (before) and
0.7.14 (after). Every row below is a case in `crates/persistant-sqlx/tests/decode_types.rs`.

Asked for by the PM after the lightbulb lane found no UUID case and worked around it with `id::text`.

**Classes.** *Carried*: returns the exact value. *Loud*: fails with `RelError::Unsupported` naming the
type (`use PgBackend::pool`). *Silent*: returns a value that is not what the database holds. Silent is
the dangerous class.

## What 0.7.12 did

Only the types `Value` already had were carried; everything else was loud, with two exceptions that
were silent:

| Backend | Type | Before | Class |
|---|---|---|---|
| PostgreSQL, MySQL | `real` / `FLOAT` | `0.1` read back as `0.10000000149011612` (widened to `f64`) | silent |
| MySQL, MariaDB | `TINYINT(1)` / `BOOLEAN` holding 5 or -3 | `Bool(true)` (sqlx names every `TINYINT(1)` BOOLEAN; nonzero is true) | silent |
| MariaDB | `UUID`, `INET4`, `INET6`, `JSON` | `Bytes` of the text form (sent with the binary character set) | silent, **not fixable here** (below) |
| PostgreSQL | `char(n)` | loud, although the code meant to carry it (it matched `BPCHAR`; sqlx names it `CHAR`) | loud (a bug) |

## What 0.7.14 does

`Value` is unchanged. The types a consumer commonly holds are carried as **canonical text**, the one
form that loses nothing: `uuid` lowercase hyphenated; `numeric`/`DECIMAL` exact digits with the
column's scale (`-0.50`, never a float); dates and times ISO 8601 (`2026-10-17`, `12:34:56.789`,
`2026-10-17T12:34:56.789`; `timestamptz` and MySQL `TIMESTAMP` in UTC with `Z`); JSON as its text;
`inet`/`cidr` as PostgreSQL prints them.

| Type | PostgreSQL | MySQL | MariaDB | SQLite |
|---|---|---|---|---|
| bool / `BOOLEAN` | `Bool` | `Bool` for 0 and 1, `Int` for 2..=127, loud above (signed or unsigned unknown) | same | stored `Int` |
| smallint, int, bigint | `Int` | `Int` | `Int` | `Int` |
| unsigned ints | n/a | `Int`; `BIGINT UNSIGNED` above `i64::MAX` is **loud** | same | n/a |
| `real` / `FLOAT` | `Float`, shortest decimal | same | same | `Float` |
| `double` | `Float` | `Float` | `Float` | `Float` |
| `uuid` | **Text** | n/a | **Bytes of its text** (`UUID` type) | whatever was stored (`Text`, or `Bytes`) |
| `numeric` / `DECIMAL`, `sum`/`avg` | **Text**, exact | **Text**, exact | **Text**, exact | stored `Int`/`Float` (SQLite has no decimal) |
| `date` | **Text** | **Text** | **Text** | stored `Text` |
| `time` | **Text** | **Text** (`-838:59:59` .. `838:59:59`) | **Text** | stored `Text` |
| `timestamp` / `DATETIME` | **Text**, no zone | **Text**, no zone | **Text** | stored `Text` |
| `timestamptz` / MySQL `TIMESTAMP` | **Text**, UTC `Z` | **Text**, UTC `Z` | **Text**, UTC `Z` | stored `Text` |
| `json`, `jsonb` / `JSON` | **Text** (`jsonb` as the server normalises it) | **Text** | **Bytes of its text** | stored `Text` |
| `inet`, `cidr` | **Text** | n/a | `INET4`/`INET6`: **Bytes of text** | n/a |
| `ENUM`, `SET` | n/a (enum types: loud) | `Text` | `Text` | n/a |
| `YEAR` | n/a | `Int` | `Int` | n/a |
| `text`, `varchar`, `char(n)`, `name` | `Text` (`char(n)` padded, as the server returns it) | `Text` | `Text` | `Text` |
| `bytea` / `BLOB` / `BINARY` | `Bytes` | `Bytes` | `Bytes` | `Bytes` |
| `interval`, `timetz`, arrays (`int4[]`, `text[]`, `uuid[]`), `macaddr`, `money`, `oid`, `xml`, `bit`, `point`, ranges, `tsvector`, enum types, `record` | **loud**, naming the type | | | |
| `BIT`, `GEOMETRY` | | **loud** | **loud** | |

SQLite types the **value**, not the column: a `DATE`, `UUID` or `JSON` column returns whatever was
stored in it (text, or a blob for a 16-byte uuid), and an `INTEGER` column holding text returns that
text. That is SQLite's own rule and is pinned by the tests, not changed.

## Not fixable at this layer

MariaDB's `UUID`, `INET4`, `INET6` and `JSON` columns arrive with the binary character set, which sqlx
reports as a binary type, so they decode as `Bytes` of their ASCII text and cannot be told from a
`BINARY(36)` holding the same bytes. A consumer on MariaDB reads them with `CAST(c AS CHAR)`, or uses
`MysqlBackend::pool`. Pinned by the MariaDB test so a change is noticed.

## Writing

`Value::Text` is bound as text. A typed column needs the cast in the statement:
`INSERT INTO t VALUES ($1::uuid)` (PostgreSQL), and a value read as text goes back in unchanged
(tested for `uuid`, `numeric`, `timestamptz`, `jsonb`, `inet`). MySQL converts text to the column type
itself.

## Found on the way

sqlx 0.9.0 `MySqlTime::is_negative()` returns `is_positive()`; the decoder asks `sign()` instead.
