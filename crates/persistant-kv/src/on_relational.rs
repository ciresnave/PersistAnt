// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{KeyPage, KvBackend, KvError};
use persistant_relational::{Dialect, RelError, RelationalBackend, Value};
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
/// The statements are written once, in PostgreSQL SQL (the source dialect), and the adapter
/// refuses any other dialect until a translator (UniSQL) is available to it as a published
/// dependency. Nothing here is *on* by derivation: each capability is certified per
/// `(server, version, config)` by [`crate::conformance`].
#[derive(Debug)]
pub struct KvOnRelational<R> {
    db: R,
    name: String,
    version: String,
}

const NOW: &str = "(extract(epoch from clock_timestamp()) * 1000)::bigint";

fn err(e: RelError) -> KvError {
    match e {
        RelError::Unsupported(m) => KvError::Unsupported(m),
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

impl<R: RelationalBackend> KvOnRelational<R> {
    /// Check the dialect, create the table if it is missing, and return the store.
    pub async fn open(db: R) -> Result<Self, KvError> {
        if db.dialect() != Dialect::Postgres {
            return Err(KvError::Unsupported(format!(
                "KvOnRelational writes PostgreSQL SQL only; {} needs a translator",
                db.dialect()
            )));
        }
        db.execute(
            "CREATE TABLE IF NOT EXISTS persistant_kv (\
                k BYTEA PRIMARY KEY, v BYTEA NOT NULL, expires_ms BIGINT)",
            &[],
        )
        .await
        .map_err(err)?;
        let name = format!("kv-on-relational:{}", db.backend());
        let version = format!("{}+{}", env!("CARGO_PKG_VERSION"), db.version());
        Ok(KvOnRelational { db, name, version })
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
        let rows = self.db.query(&sql, &params).await.map_err(err)?;
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

impl<R: RelationalBackend> KvBackend for KvOnRelational<R> {
    fn backend(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        let rows = self
            .db
            .query(
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
        self.db
            .execute(
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
        self.db
            .execute(
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
        // Inserts, or reclaims an expired row; a live row makes the DO UPDATE's WHERE false, so
        // no row is affected. Concurrent callers serialise on the row.
        let n = self
            .db
            .execute(
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
            .db
            .execute(
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
            .db
            .execute(
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
