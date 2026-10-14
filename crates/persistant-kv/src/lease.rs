// SPDX-License-Identifier: MIT OR Apache-2.0
//! `lock.lease`: an exclusive lease with an owner token and an expiry, safely reclaimed.
//!
//! Built only from three [`KvBackend`] primitives, each of which must be **on** for the backend
//! (their tests, not this module, say so): `put_if_absent` with a time to live (an expired entry
//! counts as absent), `compare_and_swap` (renew) and `compare_and_delete` (release). Reclaim after
//! expiry is the backend's `put_if_absent` finding the entry gone; nobody deletes another
//! owner's lease, and a former owner can neither renew nor release the new owner's lease.
//!
//! On a backend without those primitives every call returns [`KvError::Unsupported`]; a lease is
//! not approximated. In particular [`crate::KvOnBlob`] cannot offer it (no compare-and-delete).

use crate::{KvBackend, KvError};
use std::time::Duration;

/// A held lease. Dropping it does **not** release it; it expires, or [`Lease::release`] is called.
#[derive(Debug)]
pub struct Lease<'a, K> {
    kv: &'a K,
    key: String,
    owner: String,
}

impl<'a, K: KvBackend> Lease<'a, K> {
    /// Try to take the lease on `key` for `owner` for `ttl`. `Ok(None)`: another owner holds it.
    pub async fn acquire(
        kv: &'a K,
        key: &str,
        owner: &str,
        ttl: Duration,
    ) -> Result<Option<Lease<'a, K>>, KvError> {
        if kv
            .put_if_absent(key, owner.as_bytes().to_vec(), Some(ttl))
            .await?
        {
            Ok(Some(Lease {
                kv,
                key: key.to_string(),
                owner: owner.to_string(),
            }))
        } else {
            Ok(None)
        }
    }

    /// Extend the lease by `ttl` from now if this owner still holds it. `Ok(false)`: it expired
    /// and was taken by someone else (or is gone); this owner must stop acting as the holder.
    pub async fn renew(&self, ttl: Duration) -> Result<bool, KvError> {
        let me = self.owner.as_bytes();
        self.kv
            .compare_and_swap(&self.key, me, me.to_vec(), Some(ttl))
            .await
    }

    /// Give the lease up if this owner still holds it. `Ok(false)`: it was not held by this owner.
    pub async fn release(self) -> Result<bool, KvError> {
        self.kv
            .compare_and_delete(&self.key, self.owner.as_bytes())
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
}
