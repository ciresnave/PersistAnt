// SPDX-License-Identifier: MIT OR Apache-2.0
//! The registry for synchronous callers (feature `registry`): `providers` is already a plain function,
//! so it is re-exported unchanged, and [`open_named`] returns a [`KvHandle`] whose methods block
//! instead of returning futures.
//!
//! It is built on the same worker as [`super::Store`]: the handle owns one thread running a private
//! single-threaded runtime, a call sends its work there and waits for the reply, and nothing is
//! driven on the calling thread. So a call is safe from plain code, from many threads at once and
//! from inside someone else's runtime (it blocks that task's thread, as any blocking call does:
//! use the async [`crate::registry`] there). One worker serves every call of one handle: the
//! calls run concurrently on it, as tasks, not one after another.
//!
//! Keep **one** handle for the life of the program, the way you would keep one pool. Opening per
//! call starts a thread and a runtime each time (about 11 ms on the older `Store` facade, measured
//! by agentlife).
//!
//! A failure of the worker itself (it is gone, a task panicked) is reported as
//! [`KvError::Backend`] with the reason; an operation's own errors are returned as they are.

use std::sync::Arc;
use std::time::Duration;

use crate::capability::provider::ProviderNeeds;
use crate::kv::{BatchOp, KeyPage, KvBackend, KvError, ValuePage};
use crate::registry::{Endpoint, Handle, OpenError, OpenOptions};

pub use crate::registry::{Listing, providers, providers_ranked};

use super::{Worker, on_worker};

/// An opened backend for synchronous code. Methods are those of [`KvBackend`], blocking.
pub struct KvHandle {
    inner: Option<Arc<Handle>>,
    worker: Worker,
}

impl std::fmt::Debug for KvHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blocking::registry::KvHandle")
            .field("id", &self.id())
            .finish_non_exhaustive()
    }
}

/// As [`crate::registry::open_named`], blocking.
pub fn open_named(
    id: &str,
    endpoint: &Endpoint,
    needs: &ProviderNeeds,
) -> Result<KvHandle, OpenError> {
    open_named_with(id, endpoint, needs, &OpenOptions::new())
}

/// As [`crate::registry::open_named_with`], blocking.
pub fn open_named_with(
    id: &str,
    endpoint: &Endpoint,
    needs: &ProviderNeeds,
    options: &OpenOptions,
) -> Result<KvHandle, OpenError> {
    if on_worker() {
        return Err(OpenError::Open(
            "a blocking call from a persistant worker thread".to_string(),
        ));
    }
    let worker = Worker::start().map_err(|e| OpenError::Open(e.to_string()))?;
    let (id, endpoint, needs, options) = (
        id.to_string(),
        endpoint.clone(),
        needs.clone(),
        options.clone(),
    );
    // The open's own error is kept; only a failure of the worker is turned into text.
    let opened = worker
        .run(async move {
            Ok(
                crate::registry::open_named_with(&id, &endpoint, &needs, &options)
                    .await
                    .map(Arc::new),
            )
        })
        .map_err(|e| OpenError::Open(e.to_string()))??;
    Ok(KvHandle {
        inner: Some(opened),
        worker,
    })
}

impl KvHandle {
    fn handle(&self) -> &Arc<Handle> {
        self.inner.as_ref().expect("present until dropped")
    }

    /// The id of the backend that was opened (the string stored in configuration).
    pub fn id(&self) -> &'static str {
        self.handle().id()
    }

    /// The backend's name and version as the matrix has them.
    pub fn backend(&self) -> (String, String) {
        let kv = self.handle().kv();
        (kv.backend().to_string(), kv.version().to_string())
    }

    /// Run `f` on the worker against the shared handle and wait for its result.
    fn call<T, F, Fut>(&self, f: F) -> Result<T, KvError>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Handle>) -> Fut,
        Fut: std::future::Future<Output = Result<T, KvError>> + Send + 'static,
    {
        let fut = f(self.handle().clone());
        self.worker
            .run(async move { Ok(fut.await) })
            .map_err(|e| KvError::Backend(e.to_string()))?
    }

    /// See [`KvBackend::get`].
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().get(&key).await })
    }

    /// See [`KvBackend::put`].
    pub fn put(&self, key: &str, value: Vec<u8>, ttl: Option<Duration>) -> Result<(), KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().put(&key, value, ttl).await })
    }

    /// See [`KvBackend::delete`].
    pub fn delete(&self, key: &str) -> Result<(), KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().delete(&key).await })
    }

    /// See [`KvBackend::put_if_absent`].
    pub fn put_if_absent(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().put_if_absent(&key, value, ttl).await })
    }

    /// See [`KvBackend::prefix_list`].
    pub fn prefix_list(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        let (prefix, after) = (prefix.to_string(), after.map(str::to_string));
        self.call(|h| async move { h.kv().prefix_list(&prefix, after.as_deref(), limit).await })
    }

    /// See [`KvBackend::ordered_scan`].
    pub fn ordered_scan(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<KeyPage, KvError> {
        let (prefix, after) = (prefix.to_string(), after.map(str::to_string));
        self.call(|h| async move { h.kv().ordered_scan(&prefix, after.as_deref(), limit).await })
    }

    /// See [`KvBackend::ordered_scan_values`].
    pub fn ordered_scan_values(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<ValuePage, KvError> {
        let (prefix, after) = (prefix.to_string(), after.map(str::to_string));
        self.call(|h| async move {
            h.kv()
                .ordered_scan_values(&prefix, after.as_deref(), limit)
                .await
        })
    }

    /// See [`KvBackend::compare_and_swap`].
    pub fn compare_and_swap(
        &self,
        key: &str,
        expected: &[u8],
        new: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<bool, KvError> {
        let (key, expected) = (key.to_string(), expected.to_vec());
        self.call(|h| async move { h.kv().compare_and_swap(&key, &expected, new, ttl).await })
    }

    /// See [`KvBackend::compare_and_delete`].
    pub fn compare_and_delete(&self, key: &str, expected: &[u8]) -> Result<bool, KvError> {
        let (key, expected) = (key.to_string(), expected.to_vec());
        self.call(|h| async move { h.kv().compare_and_delete(&key, &expected).await })
    }

    /// See [`KvBackend::take`].
    pub fn take(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().take(&key).await })
    }

    /// See [`KvBackend::count`].
    pub fn count(&self, prefix: &str) -> Result<u64, KvError> {
        let prefix = prefix.to_string();
        self.call(|h| async move { h.kv().count(&prefix).await })
    }

    /// See [`KvBackend::ttl_remaining`].
    pub fn ttl_remaining(&self, key: &str) -> Result<Option<Duration>, KvError> {
        let key = key.to_string();
        self.call(|h| async move { h.kv().ttl_remaining(&key).await })
    }

    /// See [`KvBackend::purge_expired`].
    pub fn purge_expired(&self) -> Result<u64, KvError> {
        self.call(|h| async move { h.kv().purge_expired().await })
    }

    /// See [`KvBackend::batch`].
    pub fn batch(&self, ops: &[BatchOp]) -> Result<bool, KvError> {
        let ops = ops.to_vec();
        self.call(|h| async move { h.kv().batch(&ops).await })
    }
}

impl Drop for KvHandle {
    fn drop(&mut self) {
        // The backend is dropped on the thread whose runtime it was opened in (a pool closes its
        // connections there), then the worker stops.
        if let Some(inner) = self.inner.take()
            && !on_worker()
        {
            let _ = self.worker.run(async move {
                drop(inner);
                Ok(())
            });
        }
    }
}
