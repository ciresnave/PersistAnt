// SPDX-License-Identifier: MIT OR Apache-2.0
//! `native` COPY against a real PostgreSQL: every format's bytes, COPY IN in chunks that split rows,
//! a dropped sink aborting the transaction (rows never committed), an unfinished stream leaving the
//! transaction usable, hostile identifiers, a query source, and a binary transfer between two tables.

use futures_core::Stream;
use persistant_relational::{ConnectionSecret, Isolation, RelationalBackend, RelationalTx, Value};
use persistant_sqlx::native::{
    CopyFormat, CopyIn, CopyOut, Ident, PgSession, SessionSettings, TxOptions, list, one,
};
use persistant_sqlx::{ConnectOptions, PgBackend, TlsPolicy};
use std::pin::Pin;

fn url() -> Option<String> {
    let url = std::env::var("PERSISTANT_TEST_DATABASE_URL").ok();
    if url.is_none() {
        assert!(
            std::env::var("PERSISTANT_REQUIRE_DATABASE").as_deref() != Ok("1"),
            "PERSISTANT_REQUIRE_DATABASE=1 but PERSISTANT_TEST_DATABASE_URL is not set"
        );
        eprintln!("skipped: PERSISTANT_TEST_DATABASE_URL is not set");
    }
    url
}

async fn collect<
    S: Stream<Item = Result<bytes::Bytes, persistant_relational::RelError>> + Unpin,
>(
    mut s: S,
) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(chunk) = std::future::poll_fn(|cx| Pin::new(&mut s).poll_next(cx)).await {
        out.extend_from_slice(&chunk.unwrap());
    }
    out
}

async fn count(db: &PgBackend, table: &str) -> i64 {
    match db
        .query(&format!("SELECT count(*) FROM \"{table}\""), &[])
        .await
        .unwrap()[0]
        .values[0]
    {
        Value::Int(n) => n,
        ref o => panic!("{o:?}"),
    }
}

async fn make(db: &PgBackend, table: &str) {
    db.execute(&format!("DROP TABLE IF EXISTS \"{table}\""), &[])
        .await
        .unwrap();
    db.execute(&format!("CREATE TABLE \"{table}\" (a bigint, b text)"), &[])
        .await
        .unwrap();
}

async fn session(url: &str) -> PgSession {
    PgSession::connect(
        &ConnectionSecret::literal(url.to_string()),
        &ConnectOptions::default().with_tls(TlsPolicy::Disable),
        SessionSettings::new(),
    )
    .await
    .unwrap()
}

async fn db(url: &str) -> PgBackend {
    PgBackend::connect(url).await.unwrap()
}

#[tokio::test]
async fn out_in_each_format_has_that_formats_bytes_and_null() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    make(&db, "cp_fmt").await;
    db.execute("INSERT INTO cp_fmt VALUES (1, 'foo'), (2, NULL)", &[])
        .await
        .unwrap();
    let t = Ident::new("cp_fmt").unwrap();
    let cols = [Ident::new("a").unwrap(), Ident::new("b").unwrap()];
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    let text = collect(
        tx.copy_out(&CopyOut::table_columns(&t, &cols))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(String::from_utf8(text).unwrap(), "1\tfoo\n2\t\\N\n");
    let csv = collect(
        tx.copy_out(&CopyOut::table_columns(&t, &cols).format(CopyFormat::Csv))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(String::from_utf8(csv).unwrap(), "1,foo\n2,\n");
    let bin = collect(
        tx.copy_out(&CopyOut::table(&t).format(CopyFormat::Binary))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        bin.starts_with(b"PGCOPY\n\xff\r\n\0"),
        "binary COPY signature"
    );
    // the column order given is the order out
    let swapped = [cols[1].clone(), cols[0].clone()];
    let text = collect(
        tx.copy_out(&CopyOut::table_columns(&t, &swapped))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(String::from_utf8(text).unwrap(), "foo\t1\n\\N\t2\n");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn in_takes_chunks_that_split_rows_and_finish_reports_the_rows() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    make(&db, "cp_in").await;
    let t = Ident::new("cp_in").unwrap();
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
    sink.send("1\tfo").await.unwrap();
    sink.send("o\n2\t\\N\n3\tz").await.unwrap();
    sink.send(b"\n".to_vec()).await.unwrap();
    assert_eq!(sink.finish().await.unwrap(), 3);
    // the COPY is part of the transaction: visible inside, gone on rollback
    let rows = tx.query("SELECT count(*) FROM cp_in", &[]).await.unwrap();
    assert_eq!(rows[0].values[0], Value::Int(3));
    tx.rollback().await.unwrap();
    assert_eq!(count(&db, "cp_in").await, 0);
    // control: commit keeps them
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
    sink.send("9\tnine\n").await.unwrap();
    sink.finish().await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(&db, "cp_in").await, 1);
}

#[tokio::test]
async fn a_dropped_or_aborted_sink_aborts_the_transaction() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    make(&db, "cp_drop").await;
    let t = Ident::new("cp_drop").unwrap();
    // dropped without finish: the next statement fails with the COPY's failure, nothing lands
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    {
        let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
        sink.send(
            "1	a
",
        )
        .await
        .unwrap();
    }
    let e = tx.query("SELECT 1", &[]).await.unwrap_err();
    assert!(format!("{e}").contains("COPY from stdin failed"), "{e}");
    let _ = tx.rollback().await;
    assert_eq!(
        count(&db, "cp_drop").await,
        0,
        "a half-written COPY never lands"
    );
    // explicit abort
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
    sink.send(
        "1	a
",
    )
    .await
    .unwrap();
    let _ = sink.abort("changed my mind").await;
    let _ = tx.rollback().await;
    assert_eq!(count(&db, "cp_drop").await, 0);
    // bad data is a classified error, not a hang
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
    sink.send(
        "not-a-number	x
",
    )
    .await
    .unwrap();
    assert!(sink.finish().await.is_err());
    let _ = tx.rollback().await;
    // control: the pool is healthy after all of that, and a clean COPY still lands
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let mut sink = tx.copy_in(&CopyIn::table(&t)).await.unwrap();
    sink.send(
        "5	five
",
    )
    .await
    .unwrap();
    assert_eq!(sink.finish().await.unwrap(), 1);
    tx.commit().await.unwrap();
    assert_eq!(count(&db, "cp_drop").await, 1);
}

#[tokio::test]
async fn an_unfinished_out_stream_is_drained_or_the_transaction_rolled_back() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    make(&db, "cp_big").await;
    db.execute(
        "INSERT INTO cp_big SELECT g, repeat('x', 200) FROM generate_series(1, 50000) g",
        &[],
    )
    .await
    .unwrap();
    let t = Ident::new("cp_big").unwrap();
    // drain: the transaction is usable again and the whole table still streams
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    let mut s = tx.copy_out(&CopyOut::table(&t)).await.unwrap();
    assert!(
        std::future::poll_fn(|cx| Pin::new(&mut s).poll_next(cx))
            .await
            .is_some()
    );
    s.drain().await.unwrap();
    let rows = tx.query("SELECT count(*) FROM cp_big", &[]).await.unwrap();
    assert_eq!(rows[0].values[0], Value::Int(50000));
    let all = collect(tx.copy_out(&CopyOut::table(&t)).await.unwrap()).await;
    assert_eq!(all.iter().filter(|b| **b == 10).count(), 50000);
    tx.rollback().await.unwrap();
    // dropped with data unread: the next statement fails, rollback works, the pool is healthy
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    {
        let mut s = tx.copy_out(&CopyOut::table(&t)).await.unwrap();
        assert!(
            std::future::poll_fn(|cx| Pin::new(&mut s).poll_next(cx))
                .await
                .is_some()
        );
    }
    assert!(tx.query("SELECT 1", &[]).await.is_err());
    tx.rollback().await.unwrap();
    for _ in 0..3 {
        assert_eq!(count(&db, "cp_big").await, 50000);
    }
}

#[tokio::test]
async fn hostile_names_and_a_query_source() {
    let Some(url) = url() else { return };
    let t = Ident::qualified("public", "cp\"h; DROP TABLE x --").unwrap();
    let c1 = Ident::new("we\"ird col").unwrap();
    let c2 = Ident::new("select").unwrap();
    let mut sess = session(&url).await;
    let mut tx = sess
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    tx.execute_ident("DROP TABLE IF EXISTS {0}", &[one(&t)], &[])
        .await
        .unwrap();
    tx.execute_ident(
        "CREATE TABLE {0} ({1} bigint, {2} text)",
        &[one(&t), one(&c1), one(&c2)],
        &[],
    )
    .await
    .unwrap();
    let cols = [c1.clone(), c2.clone()];
    let mut sink = tx.copy_in(&CopyIn::table_columns(&t, &cols)).await.unwrap();
    sink.send("1\tone\n2\ttwo\n").await.unwrap();
    assert_eq!(sink.finish().await.unwrap(), 2);
    let out = collect(
        tx.copy_out(
            &CopyOut::query(
                "SELECT {1} FROM {0} WHERE {2} > 1 ORDER BY {2}",
                &[one(&t), list(&cols), one(&c1)],
            )
            .unwrap(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(String::from_utf8(out).unwrap(), "2\ttwo\n");
    tx.execute_ident("DROP TABLE {0}", &[one(&t)], &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_binary_transfer_between_two_tables_in_two_sessions() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    for t in ["cp_src", "cp_dst"] {
        db.execute(&format!("DROP TABLE IF EXISTS {t}"), &[])
            .await
            .unwrap();
        db.execute(
            &format!("CREATE TABLE {t} (a bigint, b text, c timestamptz, d double precision)"),
            &[],
        )
        .await
        .unwrap();
    }
    db.execute(
        "INSERT INTO cp_src SELECT g, CASE WHEN g % 3 = 0 THEN NULL ELSE 'v' || g END, \
         timestamptz '2024-03-05 06:07:08.123456+00' + g * interval '1 hour', g / 7.0 FROM generate_series(1, 1000) g",
        &[],
    )
    .await
    .unwrap();
    let opts = ConnectOptions::default().with_tls(TlsPolicy::Disable);
    let secret = ConnectionSecret::literal(url.clone());
    let settings = || {
        SessionSettings::new()
            .date_style_iso()
            .time_zone_utc()
            .extra_float_digits(3)
            .unwrap()
    };
    let mut src = PgSession::connect(&secret, &opts, settings())
        .await
        .unwrap();
    let mut dst = PgSession::connect(&secret, &opts, settings())
        .await
        .unwrap();
    let mut rd = src
        .begin(TxOptions::new(Isolation::RepeatableRead).read_only())
        .await
        .unwrap();
    let mut wr = dst
        .begin(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    let from = Ident::new("cp_src").unwrap();
    let to = Ident::new("cp_dst").unwrap();
    let mut stream = rd
        .copy_out(&CopyOut::table(&from).format(CopyFormat::Binary))
        .await
        .unwrap();
    let mut sink = wr
        .copy_in(&CopyIn::table(&to).format(CopyFormat::Binary))
        .await
        .unwrap();
    while let Some(chunk) = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_next(cx)).await {
        sink.send(chunk.unwrap()).await.unwrap();
    }
    assert_eq!(sink.finish().await.unwrap(), 1000);
    drop(stream);
    wr.commit().await.unwrap();
    rd.rollback().await.unwrap();
    let diff = db
        .query(
            "SELECT count(*) FROM (SELECT * FROM cp_src EXCEPT SELECT * FROM cp_dst) x",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        diff[0].values[0],
        Value::Int(0),
        "every source row arrived identical"
    );
    assert_eq!(count(&db, "cp_dst").await, 1000);
}

#[tokio::test]
async fn copy_is_refused_on_a_pooled_transaction() {
    let Some(url) = url() else { return };
    let db = db(&url).await;
    make(&db, "cp_pool").await;
    let t = Ident::new("cp_pool").unwrap();
    let mut tx = db
        .begin_with(TxOptions::new(Isolation::ReadCommitted))
        .await
        .unwrap();
    assert!(matches!(
        tx.copy_out(&CopyOut::table(&t)).await,
        Err(persistant_relational::RelError::Unsupported(_))
    ));
    assert!(matches!(
        tx.copy_in(&CopyIn::table(&t)).await,
        Err(persistant_relational::RelError::Unsupported(_))
    ));
    // the transaction is untouched by the refusal
    tx.query("SELECT 1", &[]).await.unwrap();
    tx.rollback().await.unwrap();
}
