// SPDX-License-Identifier: MIT OR Apache-2.0
//! A blocking facade over [`crate::Store`], for synchronous callers with no async runtime.
//!
//! Each [`Store`] owns one worker thread running a private single-threaded runtime; a call sends
//! its work to that thread over a channel and waits for the reply. The calling thread never drives
//! the runtime, so a call is safe from plain code, from inside someone else's tokio runtime of
//! either flavour (no `block_on` inside a runtime, which would panic or deadlock), and from many
//! threads at once. Inside an async task it blocks that task's thread for the duration of the
//! call, as any blocking call does: prefer the async [`crate::Store`] there.
//!
//! OpenDAL's own `blocking::Operator` was evaluated and not used: it must be created inside a
//! running tokio runtime, uses `Handle::block_on` (which panics when called from within a runtime),
//! and would put OpenDAL types in our API.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use crate::Store as AsyncStore;
use crate::error::{Error, Result};
use crate::store::{Config, Needs, Record, decode_record, encode_record};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// The worker thread and its private runtime. Dropping it stops the thread.
struct Worker {
    jobs: Option<tokio::sync::mpsc::UnboundedSender<Job>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    fn start() -> Result<Worker> {
        let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<std::result::Result<(), String>>();
        let thread = std::thread::Builder::new()
            .name("persistant-worker".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                rt.block_on(async move {
                    while let Some(job) = rx.recv().await {
                        tokio::spawn(job);
                    }
                });
            })
            .map_err(|e| Error::Backend(format!("cannot start worker thread: {e}")))?;
        // From here the Worker owns the thread, so an early return still stops it.
        let worker = Worker {
            jobs: Some(jobs),
            thread: Some(thread),
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(e)) => Err(Error::Backend(format!("cannot start runtime: {e}"))),
            Err(_) => Err(Error::Backend("worker thread died at start".into())),
        }
    }

    /// Send `fut` to the worker and wait for its output.
    fn run<Fut, T>(&self, fut: Fut) -> Result<T>
    where
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let job: Job = Box::pin(async move {
            let _ = tx.send(fut.await);
        });
        self.jobs
            .as_ref()
            .ok_or_else(|| Error::Backend("worker is gone".into()))?
            .send(job)
            .map_err(|_| Error::Backend("worker is gone".into()))?;
        rx.recv()
            .map_err(|_| Error::Backend("worker failed before replying".into()))?
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the channel ends the worker's loop; wait so its files and runtime are gone.
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A [`crate::Store`] for synchronous code. Same needs, same refusals, same atomic replace.
pub struct Store {
    inner: Arc<AsyncStore>,
    worker: Worker,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("blocking::Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Open `config` against `needs`, refusing a backend that cannot give them (see
    /// [`crate::Store::open`]).
    pub fn open(config: Config, needs: Needs) -> Result<Store> {
        let worker = Worker::start()?;
        let inner = Arc::new(worker.run(AsyncStore::open(config, needs))?);
        Ok(Store { inner, worker })
    }

    /// Read the whole value at `key`.
    pub fn read(&self, key: &str) -> Result<Vec<u8>> {
        let (s, key) = (self.inner.clone(), key.to_string());
        self.worker.run(async move { s.read(&key).await })
    }

    /// Replace the value at `key` atomically (see [`crate::Store::replace`]).
    pub fn replace(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        let (s, key) = (self.inner.clone(), key.to_string());
        self.worker.run(async move { s.replace(&key, bytes).await })
    }

    /// Delete `key`; deleting a key that does not exist is not an error.
    pub fn delete(&self, key: &str) -> Result<()> {
        let (s, key) = (self.inner.clone(), key.to_string());
        self.worker.run(async move { s.delete(&key).await })
    }

    /// Store `record` under `key` (see [`crate::Store::put_record`]).
    pub fn put_record<R: Record>(&self, key: &str, record: &R) -> Result<()> {
        self.replace(key, encode_record(record)?)
    }

    /// Read the record at `key`, `None` if absent (see [`crate::Store::get_record`]). Decoding,
    /// including a [`Record::upgrade`], runs on the calling thread, not the worker.
    pub fn get_record<R: Record>(&self, key: &str) -> Result<Option<R>> {
        let (s, key) = (self.inner.clone(), key.to_string());
        match self
            .worker
            .run(async move { s.read_optional(&key).await })?
        {
            Some(bytes) => decode_record(&bytes).map(Some),
            None => Ok(None),
        }
    }
}
