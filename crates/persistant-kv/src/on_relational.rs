// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::translate::{self, Translator};
use crate::{BatchOp, BoundedProbe, KeyPage, KvBackend, KvError, ValuePage};
use persistant_relational::{
    Dialect, Isolation, RelError, RelationalBackend, RelationalTx, Row, Value,
};
use std::collections::BTreeSet;
use std::time::Duration;

/// A [`KvBackend`] built natively on any [`RelationalBackend`] (D1, C+: "KV native on a relational
/// backend"). A wrapper type, not a blanket impl, like [`crate::KvOnBlob`].
///
/// One table, `persistant_kv(k BYTEA PRIMARY KEY, v BYTEA, expires_ms BIGINT)`. **Keys are
/// `BYTEA`, never text**: a text column orders by the database's collation, which is wrong for
/// `kv.ordered_scan` (PostgreSQL follows the database locale; MySQL's default collation is even
/// case-insensitive). Expiry is judged by the **server's clock**, so clients with skewed clocks
/// agree.
///
/// The statements are written once, in PostgreSQL SQL (the source dialect), and translated per
/// target by UniSQL: PostgreSQL (unchanged) and SQLite (3.42 or later). Any other dialect is
/// refused until UniSQL gives every construct the adapter uses. Nothing here is *on* by derivation: each capability is certified per
/// `(server, version, config)` by [`crate::conformance`].
#[derive(Debug)]
pub struct KvOnRelational<R> {
    db: R,
    tr: Translator,
    name: String,
    version: String,
}

/// How many times `batch` runs a batch the server aborted as a deadlock or serialisation victim.
const BATCH_ATTEMPTS: usize = 16;

/// Why one attempt at a batch did not finish.
enum BatchFail {
    /// The server rolled the transaction back and says to run it again.
    Retry,
    Kv(KvError),
}

impl From<RelError> for BatchFail {
    fn from(e: RelError) -> Self {
        match e {
            RelError::Deadlock | RelError::Serialization => BatchFail::Retry,
            other => BatchFail::Kv(err(other)),
        }
    }
}

const NOW: &str = "CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)";

fn err(e: RelError) -> KvError {
    match e {
        RelError::Unsupported(m) => KvError::Unsupported(m),
        RelError::Timeout { after } => KvError::TimedOut(after),
        other => KvError::Backend(other.to_string()),
    }
}

fn ttl_ms(ttl: Option<Duration>) -> Value {
    // -1: no expiry. A zero or sub-millisecond ttl is one millisecond, as in `KvOnBlob`.
    Value::Int(ttl.map_or(-1, |d| {
        i64::try_from(d.as_millis().max(1)).unwrap_or(i64::MAX / 2)
    }))
}

/// The smallest byte string greater than every string that starts with `prefix`; `None` if there
/// is none (empty prefix, or all `0xFF`).
fn upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut b = prefix.to_vec();
    while let Some(last) = b.pop() {
        if last < 0xFF {
            b.push(last + 1);
            return Some(b);
        }
    }
    None
}

/// Create the tables if they are missing. On PostgreSQL `CREATE TABLE IF NOT EXISTS` is not safe
/// against an identical statement running at the same time: the loser fails inside the catalog with
/// a unique violation on `pg_type_typname_nsp_index` or "type ... already exists". So the statements
/// run in one transaction behind a transaction-scoped advisory lock (fixed key, released at commit),
/// and concurrent first opens take turns; the later ones find the tables and do nothing. Other
/// dialects need no lock.
async fn create_tables<R: RelationalBackend>(
    db: &R,
    dialect: &Dialect,
    statements: &[&str],
) -> Result<(), KvError> {
    if *dialect != Dialect::Postgres {
        for sql in statements {
            db.execute(sql, &[]).await.map_err(err)?;
        }
        return Ok(());
    }
    let mut tx = db.begin(Isolation::ReadCommitted).await.map_err(err)?;
    tx.query(
        "SELECT 1 FROM (SELECT pg_advisory_xact_lock(7077416010261)) AS l",
        &[],
    )
    .await
    .map_err(err)?;
    for sql in statements {
        tx.execute(sql, &[]).await.map_err(err)?;
    }
    tx.commit().await.map_err(err)
}

impl<R: RelationalBackend> KvOnRelational<R> {
    /// Check the dialect, create the table if it is missing, and return the store.
    pub async fn open(db: R) -> Result<Self, KvError> {
        let tr = Translator::new(db.dialect()).map_err(err)?;
        if *tr.dialect() == Dialect::Sqlite {
            // `unixepoch('now', 'subsec')` is the clock in milliseconds; it needs SQLite 3.42.
            let v: Vec<u32> = db
                .version()
                .split('.')
                .filter_map(|p| p.parse().ok())
                .collect();
            if v.len() < 2 || (v[0], v[1]) < (3, 42) {
                return Err(KvError::Unsupported(format!(
                    "KvOnRelational on SQLite needs 3.42 or later (unixepoch subsec); this is {}",
                    db.version()
                )));
            }
        }
        let create = translate::ddl(tr.dialect()).ok_or_else(|| {
            KvError::Unsupported(format!("no table definition for {}", tr.dialect()))
        })?;
        // The second table holds the row every `batch` locks first (see `batch_once`). Plain SQL, the
        // same on every dialect.
        create_tables(
            &db,
            tr.dialect(),
            &[
                create,
                "CREATE TABLE IF NOT EXISTS persistant_kv_batch (id BIGINT NOT NULL PRIMARY KEY)",
            ],
        )
        .await?;
        match db
            .execute("INSERT INTO persistant_kv_batch (id) VALUES (1)", &[])
            .await
        {
            Ok(_) | Err(RelError::UniqueViolation { .. }) => {}
            Err(e) => return Err(err(e)),
        }
        let name = format!("kv-on-relational:{}", db.backend());
        let version = format!(
            "{}.{}+{}",
            env!("CARGO_PKG_VERSION_MAJOR"),
            env!("CARGO_PKG_VERSION_MINOR"),
            db.version()
        );
        Ok(KvOnRelational {
            db,
            tr,
            name,
            version,
        })
    }

    async fn exec(&self, sql: &str, params: &[Value]) -> Result<u64, RelError> {
        let (sql, params) = self.tr.prepare(sql, params)?;
        self.db.execute(&sql, &params).await
    }

    async fn qry(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RelError> {
        let (sql, params) = self.tr.prepare(sql, params)?;
        self.db.query(&sql, &params).await
    }

    async fn tx_exec<T: RelationalTx>(
        &self,
        tx: &mut T,
        sql: &str,
        params: &[Value],
    ) -> Result<u64, RelError> {
        let (sql, params) = self.tr.prepare(sql, params)?;
        tx.execute(&sql, &params).await
    }

    async fn tx_qry<T: RelationalTx>(
        &self,
        tx: &mut T,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Row>, RelError> {
        let (sql, params) = self.tr.prepare(sql, params)?;
        tx.query(&sql, &params).await
    }

    /// Store `value` under `key` only if the key is absent or expired; `true` if this call did.
    ///
    /// MySQL cannot say. sqlx connects with `CLIENT_FOUND_ROWS`, under which an upsert that
    /// changed nothing reports 1, the same as an insert, so the single conditional upsert the other
    /// dialects use cannot tell "stored" from "a live value was there". Here it is two statements
    /// whose counts are unambiguous under that flag: reclaim an expired row (an `UPDATE`, which
    /// counts rows *matched*), else a plain `INSERT` whose duplicate-key error means the key is
    /// live. Concurrent callers serialise on the row: the second finds it no longer expired.
    async fn claim_two_step<T: RelationalTx>(
        &self,
        tx: &mut T,
        key: &str,
        value: &[u8],
        ttl: Option<Duration>,
    ) -> Result<bool, RelError> {
        let reclaimed = self
            .tx_exec(
                tx,
                &format!(
                    "UPDATE persistant_kv SET v = $2, \
                     expires_ms = CASE WHEN $3 < 0 THEN NULL ELSE {NOW} + $3 END \
                     WHERE k = $1 AND expires_ms IS NOT NULL AND expires_ms <= {NOW}"
                ),
                &[
                    Value::Bytes(key.as_bytes().to_vec()),
                    Value::Bytes(value.to_vec()),
                    ttl_ms(ttl),
                ],
            )
            .await?;
        if reclaimed == 1 {
            return Ok(true);
        }
        match self
            .tx_exec(
                tx,
                &format!(
                    "INSERT INTO persistant_kv (k, v, expires_ms) \
                     VALUES ($1, $2, CASE WHEN $3 < 0 THEN NULL ELSE {NOW} + $3 END)"
                ),
                &[
                    Value::Bytes(key.as_bytes().to_vec()),
                    Value::Bytes(value.to_vec()),
                    ttl_ms(ttl),
                ],
            )
            .await
        {
            Ok(n) => Ok(n == 1),
            Err(RelError::UniqueViolation { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    async fn batch_once(&self, ops: &[BatchOp]) -> Result<bool, BatchFail> {
        let mut tx = self.db.begin(Isolation::ReadCommitted).await?;
        // One batch at a time on a server with row locks. The preconditions below read rows that
        // may not exist, and a lock on a missing row (a gap lock on MySQL, an in-flight insert on
        // PostgreSQL) cannot exclude a second batch, so racing batches took locks in opposite orders
        // and the server aborted one as a deadlock victim. SQLite's write transaction already
        // excludes. The retry in `batch` is the backstop, not the plan: a deadlock costs the
        // server's detection time.
        if *self.tr.dialect() != Dialect::Sqlite {
            tx.query(
                "SELECT id FROM persistant_kv_batch WHERE id = 1 FOR UPDATE",
                &[],
            )
            .await?;
        }
        // Keys a precondition says are absent: their `Put` must not overwrite a row another
        // writer inserted after the check, so it is an insert-or-reclaim-expired, and a miss
        // refuses the batch.
        let mut required_absent: BTreeSet<&str> = BTreeSet::new();
        for op in ops {
            match op {
                BatchOp::Require { key, expected } => {
                    let rows = self
                        .tx_qry(
                            &mut tx,
                            &format!(
                                "SELECT v FROM persistant_kv WHERE k = $1 \
                                 AND (expires_ms IS NULL OR expires_ms > {NOW}){lock}",
                                lock = self.lock_clause()
                            ),
                            &[Value::Bytes(key.as_bytes().to_vec())],
                        )
                        .await?;
                    let current = rows.first().and_then(|r| r.bytes(0));
                    if current != expected.as_deref() {
                        tx.rollback().await?;
                        return Ok(false);
                    }
                    if expected.is_none() {
                        required_absent.insert(key.as_str());
                    }
                }
                BatchOp::Put { key, value, ttl } => {
                    if matches!(self.tr.dialect(), Dialect::MySql | Dialect::MariaDb)
                        && required_absent.contains(key.as_str())
                    {
                        if !self.claim_two_step(&mut tx, key, value, *ttl).await? {
                            tx.rollback().await?;
                            return Ok(false);
                        }
                        continue;
                    }
                    let insert = format!(
                        "INSERT INTO persistant_kv (k, v, expires_ms) \
                         VALUES ($1, $2, CASE WHEN $3 < 0 THEN NULL ELSE {NOW} + $3 END) \
                         ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v, expires_ms = EXCLUDED.expires_ms"
                    );
                    let insert_if_absent = format!(
                        "{insert} \
                         WHERE persistant_kv.expires_ms IS NOT NULL AND persistant_kv.expires_ms <= {NOW}"
                    );
                    let sql = if required_absent.contains(key.as_str()) {
                        &insert_if_absent
                    } else {
                        &insert
                    };
                    let n = self
                        .tx_exec(
                            &mut tx,
                            sql,
                            &[
                                Value::Bytes(key.as_bytes().to_vec()),
                                Value::Bytes(value.clone()),
                                ttl_ms(*ttl),
                            ],
                        )
                        .await?;
                    if required_absent.contains(key.as_str()) && n != 1 {
                        tx.rollback().await?;
                        return Ok(false);
                    }
                }
                BatchOp::Delete { key } => {
                    self.tx_exec(
                        &mut tx,
                        "DELETE FROM persistant_kv WHERE k = $1",
                        &[Value::Bytes(key.as_bytes().to_vec())],
                    )
                    .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(true)
    }

    /// SQLite has no row locks; its write transaction is already exclusive (`BEGIN IMMEDIATE`).
    fn lock_clause(&self) -> &'static str {
        if *self.tr.dialect() == Dialect::Sqlite {
            ""
        } else {
            " FOR UPDATE"
        }
    }

    /// The underlying relational backend.
    pub fn backend_handle(&self) -> &R {
        &self.db
    }

    async fn list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        let mut sql = format!(
            "SELECT k FROM persistant_kv WHERE k >= $1 AND (expires_ms IS NULL OR expires_ms > {NOW})"
        );
        let mut params = vec![Value::Bytes(prefix.as_bytes().to_vec())];
        if let Some(upper) = upper_bound(prefix.as_bytes()) {
            params.push(Value::Bytes(upper));
            sql.push_str(&format!(" AND k < ${}", params.len()));
        }
        if let Some(a) = after {
            params.push(Value::Bytes(a.as_bytes().to_vec()));
            sql.push_str(&format!(" AND k > ${}", params.len()));
        }
        let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        params.push(Value::Int(fetch));
        sql.push_str(&format!(" ORDER BY k LIMIT ${}", params.len()));
        let rows = self.qry(&sql, &params).await.map_err(err)?;
        let mut keys = Vec::with_capacity(rows.len());
        for r in &rows {
            let bytes = r
                .bytes(0)
                .ok_or_else(|| KvError::Corrupt("key column is not bytea".to_string()))?;
            keys.push(
                String::from_utf8(bytes.to_vec())
                    .map_err(|_| KvError::Corrupt("key is not UTF-8".to_string()))?,
            );
        }
        let more = keys.len() > limit;
        keys.truncate(limit);
        let next = if more { keys.last().cloned() } else { None };
        Ok(KeyPage::new(keys, next))
    }
}

impl<R: RelationalBackend + 'static> KvBackend for KvOnRelational<R> {
    fn backend(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn call_bound(&self) -> Option<Duration> {
        self.db.call_bound()
    }

    async fn bounded_call_probe(&self) -> Result<BoundedProbe, KvError> {
        let bound = self
            .db
            .call_bound()
            .ok_or_else(|| KvError::Unsupported("no call timeout configured".to_string()))?;
        // The row every batch locks first (SQLite: its one writer lock, taken by a write to the row).
        let lock = if *self.tr.dialect() == Dialect::Sqlite {
            "UPDATE persistant_kv_batch SET id = id WHERE id = 1"
        } else {
            "SELECT id FROM persistant_kv_batch WHERE id = 1 FOR UPDATE"
        };
        let key = "persistant-conformance/kv.bounded_call/blocked";
        let seen: std::sync::Mutex<Option<(Duration, Result<bool, KvError>)>> =
            std::sync::Mutex::new(None);
        let during = Box::pin(async {
            let started = std::time::Instant::now();
            let r = self
                .batch(&[BatchOp::Put {
                    key: key.to_string(),
                    value: b"blocked".to_vec(),
                    ttl: None,
                }])
                .await;
            *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some((started.elapsed(), r));
        });
        let at_most = bound * 6 + Duration::from_secs(1);
        self.db
            .with_lock_held(lock, at_most, during)
            .await
            .map_err(err)?;
        let seen = seen.into_inner().unwrap_or_else(|e| e.into_inner());
        let (finished, elapsed, error) = match seen {
            Some((elapsed, Ok(_))) => (true, elapsed, None),
            Some((elapsed, Err(e))) => (true, elapsed, Some(e)),
            None => (false, at_most, None),
        };
        // The block is gone. The cut-off batch must not have been applied (a dropped transaction
        // rolls back), and a normal batch must work again; a statement queued behind the lock may
        // still be draining, so the second is tried a few times.
        let applied_after = self.get(key).await?.is_some();
        let mut recovered = false;
        for _ in 0..10 {
            let ok = self
                .batch(&[BatchOp::Put {
                    key: format!("{key}-after"),
                    value: b"after".to_vec(),
                    ttl: None,
                }])
                .await;
            if matches!(ok, Ok(true)) {
                recovered = true;
                break;
            }
        }
        let _ = self.delete(key).await;
        let _ = self.delete(&format!("{key}-after")).await;
        Ok(BoundedProbe {
            bound,
            elapsed,
            finished,
            error,
            applied_after,
            recovered,
        })
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        let rows = self
            .qry(
                &format!(
                    "SELECT v FROM persistant_kv WHERE k = $1 \
                     AND (expires_ms IS NULL OR expires_ms > {NOW})"
                ),
                &[Value::Bytes(key.as_bytes().to_vec())],
            )
            .await
            .map_err(err)?;
        Ok(rows.first().and_then(|r| r.bytes(0)).map(<[u8]>::to_vec))
    }

    async fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        self.exec(
            &format!(
                "INSERT INTO persistant_kv (k, v, expires_ms) \
                     VALUES ($1, $2, CASE WHEN $3 < 0 THEN NULL ELSE {NOW} + $3 END) \
                     ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v, expires_ms = EXCLUDED.expires_ms"
            ),
            &[
                Value::Bytes(key.as_bytes().to_vec()),
                Value::Bytes(value),
                ttl_ms(ttl),
            ],
        )
        .await
        .map_err(err)?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.exec(
            "DELETE FROM persistant_kv WHERE k = $1",
            &[Value::Bytes(key.as_bytes().to_vec())],
        )
        .await
        .map_err(err)?;
        Ok(())
    }

    async fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        if matches!(self.tr.dialect(), Dialect::MySql | Dialect::MariaDb) {
            let mut tx = self.db.begin(Isolation::ReadCommitted).await.map_err(err)?;
            let won = self
                .claim_two_step(&mut tx, key, &value, ttl)
                .await
                .map_err(err)?;
            tx.commit().await.map_err(err)?;
            return Ok(won);
        }
        // Inserts, or reclaims an expired row; a live row makes the DO UPDATE's WHERE false, so
        // no row is affected. Concurrent callers serialise on the row.
        let n = self
            .exec(
                &format!(
                    "INSERT INTO persistant_kv (k, v, expires_ms) \
                     VALUES ($1, $2, CASE WHEN $3 < 0 THEN NULL ELSE {NOW} + $3 END) \
                     ON CONFLICT (k) DO UPDATE SET v = EXCLUDED.v, expires_ms = EXCLUDED.expires_ms \
                     WHERE persistant_kv.expires_ms IS NOT NULL AND persistant_kv.expires_ms <= {NOW}"
                ),
                &[
                    Value::Bytes(key.as_bytes().to_vec()),
                    Value::Bytes(value),
                    ttl_ms(ttl),
                ],
            )
            .await
            .map_err(err)?;
        Ok(n == 1)
    }

    async fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let n = self
            .exec(
                &format!(
                    "UPDATE persistant_kv \
                     SET v = $3, expires_ms = CASE WHEN $4 < 0 THEN NULL ELSE {NOW} + $4 END \
                     WHERE k = $1 AND v = $2 AND (expires_ms IS NULL OR expires_ms > {NOW})"
                ),
                &[
                    Value::Bytes(key.as_bytes().to_vec()),
                    Value::Bytes(expected.to_vec()),
                    Value::Bytes(new),
                    ttl_ms(ttl),
                ],
            )
            .await
            .map_err(err)?;
        Ok(n == 1)
    }

    async fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        let n = self
            .exec(
                &format!(
                    "DELETE FROM persistant_kv \
                     WHERE k = $1 AND v = $2 AND (expires_ms IS NULL OR expires_ms > {NOW})"
                ),
                &[
                    Value::Bytes(key.as_bytes().to_vec()),
                    Value::Bytes(expected.to_vec()),
                ],
            )
            .await
            .map_err(err)?;
        Ok(n == 1)
    }

    async fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        let rows = self
            .qry(
                &format!(
                    "SELECT expires_ms - {NOW} FROM persistant_kv \
                     WHERE k = $1 AND expires_ms IS NOT NULL AND expires_ms > {NOW}"
                ),
                &[Value::Bytes(key.as_bytes().to_vec())],
            )
            .await
            .map_err(err)?;
        Ok(rows
            .first()
            .and_then(|r| r.int(0))
            .and_then(|ms| u64::try_from(ms).ok())
            .map(Duration::from_millis))
    }

    async fn purge_expired(&self) -> Result<u64, KvError> {
        self.exec(
            &format!(
                "DELETE FROM persistant_kv WHERE expires_ms IS NOT NULL AND expires_ms <= {NOW}"
            ),
            &[],
        )
        .await
        .map_err(err)
    }

    async fn count(&self, prefix: &str) -> Result<u64, KvError> {
        // The same key range as `list`, in one statement, so it is one snapshot.
        let mut sql = format!(
            "SELECT count(*) FROM persistant_kv WHERE k >= $1 AND (expires_ms IS NULL OR expires_ms > {NOW})"
        );
        let mut params = vec![Value::Bytes(prefix.as_bytes().to_vec())];
        if let Some(upper) = upper_bound(prefix.as_bytes()) {
            params.push(Value::Bytes(upper));
            sql.push_str(" AND k < $2");
        }
        let rows = self.qry(&sql, &params).await.map_err(err)?;
        rows.first()
            .and_then(|r| r.int(0))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| KvError::Corrupt("count did not return a number".to_string()))
    }

    async fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        // A deadlock or serialisation victim was rolled back whole, so the batch changed nothing and
        // running it again is the same as having started a moment later: the racer then reads the
        // winner's write and is answered `false`, rather than handed the server's abort as an error.
        for _ in 0..BATCH_ATTEMPTS {
            match self.batch_once(ops).await {
                Ok(applied) => return Ok(applied),
                Err(BatchFail::Retry) => {}
                Err(BatchFail::Kv(e)) => return Err(e),
            }
        }
        Err(KvError::Backend(format!(
            "batch: still a deadlock or serialisation victim after {BATCH_ATTEMPTS} attempts"
        )))
    }

    async fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        let mut sql = format!(
            "SELECT k, v FROM persistant_kv WHERE k >= $1 \
             AND (expires_ms IS NULL OR expires_ms > {NOW})"
        );
        let mut params = vec![Value::Bytes(prefix.as_bytes().to_vec())];
        if let Some(upper) = upper_bound(prefix.as_bytes()) {
            params.push(Value::Bytes(upper));
            sql.push_str(&format!(" AND k < ${}", params.len()));
        }
        if let Some(a) = after {
            params.push(Value::Bytes(a.as_bytes().to_vec()));
            sql.push_str(&format!(" AND k > ${}", params.len()));
        }
        let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
        params.push(Value::Int(fetch));
        sql.push_str(&format!(" ORDER BY k LIMIT ${}", params.len()));
        // One statement: one snapshot.
        let rows = self.qry(&sql, &params).await.map_err(err)?;
        let mut entries = Vec::with_capacity(rows.len());
        for r in &rows {
            let k = r
                .bytes(0)
                .and_then(|b| String::from_utf8(b.to_vec()).ok())
                .ok_or_else(|| KvError::Corrupt("key column is not UTF-8 bytea".to_string()))?;
            let v = r
                .bytes(1)
                .ok_or_else(|| KvError::Corrupt("value column is not bytea".to_string()))?
                .to_vec();
            entries.push((k, v));
        }
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next = if more {
            entries.last().map(|(k, _)| k.clone())
        } else {
            None
        };
        Ok(ValuePage::new(entries, next))
    }

    async fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.list(prefix, after, limit).await
    }

    async fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        self.list(prefix, after, limit).await
    }
}

#[cfg(test)]
mod tests {
    use super::upper_bound;

    #[test]
    fn upper_bound_increments_and_carries() {
        assert_eq!(upper_bound(b"a/"), Some(b"a0".to_vec()));
        assert_eq!(upper_bound(&[b'a', 0xFF]), Some(vec![b'b']));
        assert_eq!(upper_bound(&[0xFF, 0xFF]), None);
        assert_eq!(upper_bound(b""), None);
    }
}
