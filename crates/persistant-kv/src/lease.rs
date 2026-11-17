// SPDX-License-Identifier: MIT OR Apache-2.0
//! `lock.lease`: an exclusive lease with an owner token and an expiry, safely reclaimed; and
//! `lock.fencing`: a number, strictly greater for each later holder of one key (ROADMAP row 41, step 2).
//!
//! Built only from [`KvBackend`] primitives, each of which must be **on** for the backend (their
//! tests, not this module, say so): `put_if_absent` with a time to live (an expired entry counts as
//! absent), `compare_and_swap` (renew, and the fencing counter) and `compare_and_delete` (release).
//! Reclaim after expiry is the backend's `put_if_absent` finding the entry gone; nobody deletes
//! another owner's lease, and a former owner can neither renew nor release the new owner's lease.
//!
//! On a backend without those primitives every call returns [`KvError::Unsupported`]; a lease is
//! not approximated. In particular [`crate::KvOnBlob`] cannot offer it (no compare-and-delete).
//!
//! # The fencing token
//!
//! [`Lease::token`] is a `u64` that is **strictly greater for each later holder of the same key**.
//! It is taken from a counter key (`<key>#fence`, never expires) by [`crate::cas::update`] *after*
//! the lease is won, so only one holder at a time ever takes one, and a holder that lost the race
//! takes none. The number is then written into the lease value. A crash between the win and that
//! write leaves a lease that expires on its own with token 0 and no holder acting on it.
//!
//! **What this does and does not give.** The token is a counter, not a time, so its order does not
//! depend on any clock. The clock only decides *when* a lease may be reclaimed (the database's, for
//! the SQL backends); a pause or a clock step can let a lease expire while its holder is still
//! working, and that holder then acts as a stale one. The token protects only a resource that
//! **checks it**: the resource must remember the highest token it has seen and refuse a lower one.
//! A compare-and-swap record that carries the token ([`crate::cas::update`]) is such a resource.
//!
//! # Stored form
//!
//! The lease value is `owner`, then `\0#`, then the token in decimal. A value without that suffix
//! (written before fencing existed) reads as token 0 ([`Lease::holder`]); this version cannot renew
//! or release it, and takes the key over only after it expires. Upgrade every holder of a key
//! together.

use crate::cas;
use crate::{KvBackend, KvError};
use std::time::Duration;

const SEP: &str = "\0#";

/// The counter key for a lease key.
fn counter_key(key: &str) -> String {
    format!("{key}#fence")
}

fn encode(owner: &str, token: u64) -> Vec<u8> {
    format!("{owner}{SEP}{token}").into_bytes()
}

/// Split a stored lease value into owner and token. No suffix, or a suffix that is not a number:
/// the whole value is the owner and the token is 0.
fn decode(raw: &[u8]) -> (String, u64) {
    let text = String::from_utf8_lossy(raw);
    if let Some((owner, tok)) = text.rsplit_once(SEP)
        && let Ok(t) = tok.parse::<u64>()
    {
        return (owner.to_string(), t);
    }
    (text.into_owned(), 0)
}

fn parse_count(v: Option<&[u8]>) -> u64 {
    v.and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

fn from_update(e: cas::UpdateError) -> KvError {
    match e {
        cas::UpdateError::Kv(k) => k,
        cas::UpdateError::Contended { attempts } => KvError::Backend(format!(
            "lease: the fencing counter was contended {attempts} times in a row"
        )),
    }
}

/// Who holds a lease right now, as read from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// The owner token the holder acquired with.
    pub owner: String,
    /// The fencing token; 0 for a value written before fencing existed, or one whose holder had
    /// not yet written its token.
    pub token: u64,
}

/// A held lease. Dropping it does **not** release it; it expires, or [`Lease::release`] is called.
#[derive(Debug)]
pub struct Lease<'a, K> {
    kv: &'a K,
    key: String,
    owner: String,
    token: u64,
}

impl<'a, K: KvBackend> Lease<'a, K> {
    /// Try to take the lease on `key` for `owner` for `ttl`. `Ok(None)`: another owner holds it.
    pub async fn acquire(
        kv: &'a K,
        key: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<Lease<'a, K>>, KvError> {
        // Won with token 0; nobody else can take the key until it expires, so only this holder
        // takes a number from the counter now.
        if !kv.put_if_absent(key, encode(owner, 0), Some(ttl)).await? {
            return Ok(None);
        }
        let bytes = cas::update(kv, &counter_key(key), None, |v| {
            Some((parse_count(v) + 1).to_string().into_bytes())
        })
        .await
        .map_err(from_update)?
        .ok_or_else(|| KvError::Corrupt("lease: the fencing counter was not written".into()))?;
        let token = parse_count(Some(&bytes));
        // Put the number into the lease. False: the lease expired during acquire and someone else
        // holds it, so this call did not get it.
        if kv
            .compare_and_swap(key, &encode(owner, 0), encode(owner, token), Some(ttl))
            .await?
        {
            Ok(Some(Lease {
                kv,
                key: key.to_string(),
                owner: owner.to_string(),
                token,
            }))
        } else {
            Ok(None)
        }
    }

    /// Who holds `key` now, or `None` if it is free or expired. A value written before fencing
    /// existed reads as token 0.
    pub async fn holder(kv: &K, key: &str) -> Result<Option<Holder>, KvError> {
        Ok(kv.get(key).await?.map(|raw| {
            let (owner, token) = decode(&raw);
            Holder { owner, token }
        }))
    }

    /// Extend the lease by `ttl` from now if this owner still holds it. `Ok(false)`: it expired
    /// and was taken by someone else (or is gone); this owner must stop acting as the holder.
    /// The token does not change.
    pub async fn renew(&self, ttl: Duration) -> Result<bool, KvError> {
        let me = encode(&self.owner, self.token);
        self.kv
            .compare_and_swap(&self.key, &me, me.clone(), Some(ttl))
            .await
    }

    /// Give the lease up if this owner still holds it. `Ok(false)`: it was not held by this owner.
    pub async fn release(self) -> Result<bool, KvError> {
        self.kv
            .compare_and_delete(&self.key, &encode(&self.owner, self.token))
            .await
    }

    /// The key.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The owner token.
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// The fencing token: strictly greater than that of every earlier holder of this key. Hand it
    /// to the resource being protected, which must refuse a lower one than it has seen.
    pub fn token(&self) -> u64 {
        self.token
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_without_the_suffix_reads_as_token_zero_and_the_owner_may_contain_the_separator() {
        assert_eq!(decode(b"worker-1"), ("worker-1".to_string(), 0));
        assert_eq!(decode(&encode("worker-1", 7)), ("worker-1".to_string(), 7));
        assert_eq!(decode(&encode("a\0#b", 9)), ("a\0#b".to_string(), 9));
        assert_eq!(decode(b"x\0#notanumber"), ("x\0#notanumber".to_string(), 0));
    }

    #[tokio::test]
    async fn holder_reads_an_old_format_lease_as_token_zero_and_it_is_not_taken_over() {
        let kv = crate::MemoryKv::new();
        kv.put("k", b"old-owner".to_vec(), None).await.unwrap();
        assert_eq!(
            Lease::holder(&kv, "k").await.unwrap(),
            Some(Holder {
                owner: "old-owner".to_string(),
                token: 0
            })
        );
        assert!(
            Lease::acquire(&kv, "k", "new", Duration::from_secs(60))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn tokens_increase_across_owners_and_renew_keeps_the_token() {
        let kv = crate::MemoryKv::new();
        let long = Duration::from_secs(60);
        let a = Lease::acquire(&kv, "k", "A", long).await.unwrap().unwrap();
        assert_eq!(a.token(), 1);
        assert!(a.renew(long).await.unwrap());
        assert_eq!(a.token(), 1);
        assert!(a.release().await.unwrap());
        let b = Lease::acquire(&kv, "k", "B", long).await.unwrap().unwrap();
        assert_eq!(b.token(), 2);
        assert_eq!(
            Lease::holder(&kv, "k").await.unwrap(),
            Some(Holder {
                owner: "B".to_string(),
                token: 2
            })
        );
    }
}
