// SPDX-License-Identifier: MIT OR Apache-2.0
//! What each backend's row decoder returns for each column type a consumer can hit (the audit behind
//! `docs/DECODER-AUDIT.md`). Every row of the audit is one case here, per backend:
//!
//! - **carried**: the exact [`Value`] it must produce (a `uuid` is canonical lowercase text, a `numeric`
//!   is its exact decimal text, never a float, a timestamp is ISO 8601);
//! - **loud**: it must fail with [`RelError::Unsupported`] naming the type, never return a wrong value.
//!
//! PostgreSQL needs `PERSISTANT_TEST_DATABASE_URL`, MySQL/MariaDB `PERSISTANT_TEST_MYSQL_URL`; without
//! them these tests print a note and pass **unless** `PERSISTANT_REQUIRE_DATABASE=1` /
//! `PERSISTANT_REQUIRE_MYSQL=1`. SQLite needs no server.

use persistant_relational::{RelError, RelationalBackend, Value};

enum Want {
    /// Exactly this value.
    Is(Value),
    /// `Err(Unsupported)` whose message contains this type name.
    Loud(&'static str),
}
use Want::{Is, Loud};

fn t(s: &str) -> Want {
    Is(Value::Text(s.to_string()))
}

/// Create a table with one column of `decl`, insert `lit`, read it back, compare.
async fn check(db: &impl RelationalBackend, tag: &str, cases: &[(&str, &str, Want)]) {
    let mut failures = Vec::new();
    for (n, (decl, lit, want)) in cases.iter().enumerate() {
        let table = format!("pa_dec_{tag}_{n}");
        let _ = db
            .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
            .await;
        db.execute(&format!("CREATE TABLE {table} (c {decl})"), &[])
            .await
            .unwrap_or_else(|e| panic!("create {decl}: {e}"));
        db.execute(&format!("INSERT INTO {table} VALUES ({lit})"), &[])
            .await
            .unwrap_or_else(|e| panic!("insert {lit} into {decl}: {e}"));
        let got = db.query(&format!("SELECT c FROM {table}"), &[]).await;
        let _ = db.execute(&format!("DROP TABLE {table}"), &[]).await;
        let ok = match (&got, want) {
            (Ok(rows), Is(v)) => rows[0].values[0] == *v,
            (Err(RelError::Unsupported(m)), Loud(name)) => m.contains(name),
            _ => false,
        };
        if !ok {
            let w = match want {
                Is(v) => format!("{v:?}"),
                Loud(n) => format!("Unsupported naming {n}"),
            };
            failures.push(format!("{decl} = {lit}: want {w}, got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// A query expression (aggregates and functions have types no column declares).
async fn exprs(db: &impl RelationalBackend, cases: &[(&str, Want)]) {
    let mut failures = Vec::new();
    for (sql, want) in cases {
        let got = db.query(sql, &[]).await;
        let ok = match (&got, want) {
            (Ok(rows), Is(v)) => rows[0].values[0] == *v,
            (Err(RelError::Unsupported(m)), Loud(name)) => m.contains(name),
            _ => false,
        };
        if !ok {
            failures.push(format!("{sql}: got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[tokio::test]
async fn postgres() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_DATABASE_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
        return;
    };
    let db = persistant_sqlx::PgBackend::connect(&url).await.unwrap();
    check(
        &db,
        "pg",
        &[
            ("boolean", "true", Is(Value::Bool(true))),
            ("smallint", "-7", Is(Value::Int(-7))),
            ("integer", "123456", Is(Value::Int(123456))),
            (
                "bigint",
                "9007199254740993",
                Is(Value::Int(9007199254740993)),
            ),
            // 0.1 as a REAL is 0.1, not 0.10000000149011612.
            ("real", "0.1", Is(Value::Float(0.1))),
            ("double precision", "0.1", Is(Value::Float(0.1))),
            ("text", "'héllo'", t("héllo")),
            ("varchar(5)", "'abc'", t("abc")),
            ("char(5)", "'abc'", t("abc  ")),
            ("name", "'nm'", t("nm")),
            (
                "bytea",
                "'\\xdeadbeef'",
                Is(Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
            ),
            // uuid: canonical lowercase text.
            (
                "uuid",
                "'A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11'",
                t("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            ),
            // numeric: exact decimal text, scale preserved, never a float.
            ("numeric(10,2)", "12345.67", t("12345.67")),
            ("numeric(10,2)", "-0.5", t("-0.50")),
            ("numeric(10,2)", "0", t("0.00")),
            ("numeric", "0", t("0")),
            ("numeric", "1e30", t("1000000000000000000000000000000")),
            ("numeric", "0.000001234", t("0.000001234")),
            ("numeric", "123456789.123456789", t("123456789.123456789")),
            ("numeric", "10000", t("10000")),
            ("numeric", "'NaN'", t("NaN")),
            // date and time types: ISO 8601; timestamptz in UTC with Z.
            ("date", "'2026-10-17'", t("2026-10-17")),
            ("time", "'12:34:56'", t("12:34:56")),
            ("time", "'12:34:56.789'", t("12:34:56.789")),
            (
                "timestamp",
                "'2026-10-17 12:34:56'",
                t("2026-10-17T12:34:56"),
            ),
            (
                "timestamp",
                "'2026-10-17 12:34:56.789'",
                t("2026-10-17T12:34:56.789"),
            ),
            (
                "timestamptz",
                "'2026-10-17 12:34:56.789+02'",
                t("2026-10-17T10:34:56.789Z"),
            ),
            (
                "timestamptz",
                "'2026-10-17 00:00:00+00'",
                t("2026-10-17T00:00:00Z"),
            ),
            // json / jsonb: the JSON text (jsonb is normalised by the server).
            ("json", "'{\"a\":1}'", t("{\"a\":1}")),
            ("jsonb", "'{\"a\":1}'", t("{\"a\": 1}")),
            // inet / cidr.
            ("inet", "'10.0.0.1'", t("10.0.0.1")),
            ("inet", "'10.0.0.1/24'", t("10.0.0.1/24")),
            ("inet", "'::1'", t("::1")),
            ("cidr", "'10.0.0.0/24'", t("10.0.0.0/24")),
            // Not carried: each fails loud, naming the type.
            ("timetz", "'12:34:56+02'", Loud("TIMETZ")),
            ("interval", "'1 day 2 hours'", Loud("INTERVAL")),
            ("integer[]", "'{1,2,3}'", Loud("INT4[]")),
            ("text[]", "'{a,b}'", Loud("TEXT[]")),
            (
                "uuid[]",
                "'{a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11}'",
                Loud("UUID[]"),
            ),
            ("macaddr", "'08:00:2b:01:02:03'", Loud("MACADDR")),
            ("money", "'12.34'", Loud("MONEY")),
            ("oid", "12", Loud("OID")),
            ("xml", "'<a/>'", Loud("xml")),
            ("bit(3)", "B'101'", Loud("BIT")),
            ("point", "'(1,2)'", Loud("POINT")),
            ("int4range", "'[1,5)'", Loud("INT4RANGE")),
            ("tsvector", "to_tsvector('a b')", Loud("tsvector")),
        ],
    )
    .await;
    exprs(
        &db,
        &[
            (
                "SELECT count(*) FROM generate_series(1,3)",
                Is(Value::Int(3)),
            ),
            (
                "SELECT sum(1::int4) FROM generate_series(1,3)",
                Is(Value::Int(3)),
            ),
            ("SELECT sum(1::int8) FROM generate_series(1,3)", t("3")),
            (
                "SELECT avg(1::int4) FROM generate_series(1,3)",
                t("1.00000000000000000000"),
            ),
            ("SELECT 1.5", t("1.5")),
            (
                "SELECT 'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid",
                t("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            ),
            ("SELECT array[1,2]", Loud("INT4[]")),
            ("SELECT (1,2)", Loud("RECORD")),
        ],
    )
    .await;
    // now() is a timestamptz: UTC text ending in Z.
    let now = db.query("SELECT now()", &[]).await.unwrap();
    assert!(
        matches!(&now[0].values[0], Value::Text(s) if s.ends_with('Z') && s.contains('T')),
        "{now:?}"
    );
    // A Text parameter reaches a typed column with an explicit cast, and reads back as it went in.
    for (decl, cast, text) in [
        ("uuid", "uuid", "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
        ("numeric(10,2)", "numeric", "12345.67"),
        ("timestamptz", "timestamptz", "2026-10-17T10:34:56.789Z"),
        ("jsonb", "jsonb", "{\"a\": 1}"),
        ("inet", "inet", "10.0.0.1/24"),
    ] {
        let table = format!("pa_dec_bind_{cast}");
        let _ = db
            .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
            .await;
        db.execute(&format!("CREATE TABLE {table} (c {decl})"), &[])
            .await
            .unwrap();
        db.execute(
            &format!("INSERT INTO {table} VALUES ($1::{cast})"),
            &[Value::Text(text.to_string())],
        )
        .await
        .unwrap();
        let rows = db
            .query(&format!("SELECT c FROM {table}"), &[])
            .await
            .unwrap();
        assert_eq!(rows[0].values[0], Value::Text(text.to_string()), "{decl}");
        let _ = db.execute(&format!("DROP TABLE {table}"), &[]).await;
    }
}

#[cfg(feature = "mysql")]
#[tokio::test]
async fn mysql_or_mariadb() {
    let Ok(url) = std::env::var("PERSISTANT_TEST_MYSQL_URL") else {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_MYSQL").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_MYSQL=1 but PERSISTANT_TEST_MYSQL_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_MYSQL_URL is not set");
        return;
    };
    let db = persistant_sqlx::MysqlBackend::connect(&url).await.unwrap();
    let maria = std::env::var("PERSISTANT_TEST_MYSQL_FLAVOR").as_deref() == Ok("mariadb");
    let mut cases: Vec<(&str, &str, Want)> = vec![
        ("BOOLEAN", "true", Is(Value::Bool(true))),
        ("BOOLEAN", "false", Is(Value::Bool(false))),
        // TINYINT(1) is a boolean to sqlx, but MySQL keeps any byte there: 5 must stay 5.
        ("TINYINT(1)", "5", Is(Value::Int(5))),
        // A byte of 128 or more is signed or unsigned and the column name does not say which: loud.
        ("TINYINT(1)", "-3", Loud("TINYINT(1)")),
        ("TINYINT", "-7", Is(Value::Int(-7))),
        ("SMALLINT", "-7", Is(Value::Int(-7))),
        ("MEDIUMINT", "-7", Is(Value::Int(-7))),
        ("INT", "123456", Is(Value::Int(123456))),
        (
            "BIGINT",
            "9007199254740993",
            Is(Value::Int(9007199254740993)),
        ),
        ("TINYINT UNSIGNED", "200", Is(Value::Int(200))),
        ("SMALLINT UNSIGNED", "60000", Is(Value::Int(60000))),
        ("MEDIUMINT UNSIGNED", "16000000", Is(Value::Int(16_000_000))),
        ("INT UNSIGNED", "4000000000", Is(Value::Int(4_000_000_000))),
        (
            "BIGINT UNSIGNED",
            "9223372036854775807",
            Is(Value::Int(i64::MAX)),
        ),
        // Above i64::MAX there is no Value::Int: loud, never wrapped to a negative number.
        (
            "BIGINT UNSIGNED",
            "18446744073709551615",
            Loud("BIGINT UNSIGNED"),
        ),
        ("FLOAT", "0.1", Is(Value::Float(0.1))),
        ("DOUBLE", "0.1", Is(Value::Float(0.1))),
        ("DECIMAL(10,2)", "12345.67", t("12345.67")),
        ("DECIMAL(10,2)", "-0.5", t("-0.50")),
        (
            "DECIMAL(30,0)",
            "123456789012345678901234567890",
            t("123456789012345678901234567890"),
        ),
        ("CHAR(5)", "'abc'", t("abc")),
        ("VARCHAR(5)", "'abc'", t("abc")),
        ("TEXT", "'héllo'", t("héllo")),
        ("LONGTEXT", "'abc'", t("abc")),
        ("SET('a','b')", "'a,b'", t("a,b")),
        (
            "VARBINARY(4)",
            "x'deadbeef'",
            Is(Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
        ),
        ("BLOB", "x'de'", Is(Value::Bytes(vec![0xde]))),
        ("ENUM('a','b')", "'a'", t("a")),
        ("DATE", "'2026-10-17'", t("2026-10-17")),
        (
            "DATETIME",
            "'2026-10-17 12:34:56'",
            t("2026-10-17T12:34:56"),
        ),
        (
            "DATETIME(3)",
            "'2026-10-17 12:34:56.789'",
            t("2026-10-17T12:34:56.789"),
        ),
        (
            "TIMESTAMP",
            "'2026-10-17 12:34:56'",
            t("2026-10-17T12:34:56Z"),
        ),
        ("TIME", "'12:34:56'", t("12:34:56")),
        ("TIME", "'-838:59:59'", t("-838:59:59")),
        ("TIME(3)", "'01:02:03.456'", t("01:02:03.456")),
        ("YEAR", "2026", Is(Value::Int(2026))),
        ("BIT(8)", "b'10101010'", Loud("BIT")),
        (
            "GEOMETRY",
            "ST_GeomFromText('POINT(1 2)')",
            Loud("GEOMETRY"),
        ),
    ];
    if maria {
        // MariaDB sends JSON (an alias for LONGTEXT), UUID and INET4/INET6 with the binary
        // character set, so they arrive as the bytes of their text: known, pinned, see the audit.
        cases.push((
            "JSON",
            "'{\"a\":1}'",
            Is(Value::Bytes(b"{\"a\":1}".to_vec())),
        ));
        cases.push((
            "UUID",
            "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'",
            Is(Value::Bytes(
                b"a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11".to_vec(),
            )),
        ));
        cases.push(("INET6", "'::1'", Is(Value::Bytes(b"::1".to_vec()))));
    } else {
        cases.push(("JSON", "'{\"a\":1}'", t("{\"a\": 1}")));
    }
    check(&db, "my", &cases).await;
    exprs(
        &db,
        &[
            (
                "SELECT COUNT(*) FROM (SELECT 1 UNION ALL SELECT 2) q",
                Is(Value::Int(2)),
            ),
            ("SELECT SUM(1) FROM (SELECT 1 UNION ALL SELECT 2) q", t("2")),
            (
                "SELECT AVG(1) FROM (SELECT 1 UNION ALL SELECT 2) q",
                t("1.0000"),
            ),
            ("SELECT 1.5", t("1.5")),
            ("SELECT CAST('1' AS UNSIGNED)", Is(Value::Int(1))),
        ],
    )
    .await;
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite() {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("persistant-dec-{}-{n}.db", std::process::id()));
    let db = persistant_sqlx::SqliteBackend::open(&path).await.unwrap();
    // SQLite types the value, not the column: what is stored is what comes back. Pinned so a change
    // is a decision. A uuid, date, timestamp or json is whatever the program stored (text or blob).
    check(
        &db,
        "sl",
        &[
            ("INTEGER", "42", Is(Value::Int(42))),
            ("INTEGER", "9223372036854775807", Is(Value::Int(i64::MAX))),
            (
                "INTEGER",
                "'text in an integer column'",
                t("text in an integer column"),
            ),
            ("BOOLEAN", "1", Is(Value::Int(1))),
            ("REAL", "0.1", Is(Value::Float(0.1))),
            ("REAL", "3", Is(Value::Float(3.0))),
            ("NUMERIC", "12.5", Is(Value::Float(12.5))),
            ("DECIMAL(10,2)", "12.34", Is(Value::Float(12.34))),
            ("TEXT", "'héllo'", t("héllo")),
            ("TEXT", "42", t("42")),
            ("VARCHAR(10)", "'abc'", t("abc")),
            (
                "BLOB",
                "x'deadbeef'",
                Is(Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])),
            ),
            ("DATE", "'2026-10-17'", t("2026-10-17")),
            (
                "DATETIME",
                "'2026-10-17 12:34:56'",
                t("2026-10-17 12:34:56"),
            ),
            ("JSON", "'{\"a\":1}'", t("{\"a\":1}")),
            (
                "UUID",
                "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'",
                t("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
            ),
            (
                "UUID",
                "x'a0eebc999c0b4ef8bb6d6bb9bd380a11'",
                Is(Value::Bytes(vec![
                    0xa0, 0xee, 0xbc, 0x99, 0x9c, 0x0b, 0x4e, 0xf8, 0xbb, 0x6d, 0x6b, 0xb9, 0xbd,
                    0x38, 0x0a, 0x11,
                ])),
            ),
            ("", "42", Is(Value::Int(42))),
        ],
    )
    .await;
    for ext in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        let _ = std::fs::remove_file(p);
    }
}
