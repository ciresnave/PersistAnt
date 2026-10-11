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
//! A call made *from a worker thread* of any blocking `Store` is refused with
//! [`Error::Reentrant`] instead of waiting: the worker's runtime has one thread, so it would wait
//! on a reply only it could produce. No public API of this crate runs caller code on a worker
//! today, so this guards an invariant rather than a path a caller can reach.
//!
//! OpenDAL's own `blocking::Operator` was evaluated and not used: it must be created inside a
//! running tokio runtime, uses `Handle::block_on` (which panics when called from within a runtime),
//! and would put OpenDAL types in our API.

use std::future::Future;
use std::num::NonZeroUsize;
use std::pin::Pin;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use crate::Store as AsyncStore;
use crate::error::{Error, Result};
use crate::store::{Config, Entry, Needs, Page, Record, decode_record, encode_record};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

thread_local! {
    /// True on every thread this module started as a worker. A blocking call made there would
    /// wait on a reply that only a worker thread can produce, and a worker's runtime has one thread.
    static ON_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn on_worker() -> bool {
    ON_WORKER.with(std::cell::Cell::get)
}

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
                ON_WORKER.with(|f| f.set(true));
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
        // Checked before anything is sent: waiting for the reply below would block this very
        // thread, which is the only one that could produce it.
        if on_worker() {
            return Err(Error::Reentrant);
        }
        let (tx, rx) = mpsc::channel();
        let job: Job = Box::pin(async move {
            // Run the work as its own task so a panic is caught by the runtime and reported,
            // instead of dropping `tx` and leaving the caller with no reason.
            let out = match tokio::spawn(fut).await {
                Ok(out) => out,
                Err(e) if e.is_panic() => Err(Error::Panicked(panic_text(e.into_panic()))),
                Err(e) => Err(Error::Backend(format!("operation did not complete: {e}"))),
            };
            let _ = tx.send(out);
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

/// The message of a panic payload when it is a string, else a placeholder.
fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(s) => *s,
        Err(p) => match p.downcast::<&'static str>() {
            Ok(s) => (*s).to_string(),
            Err(_) => "non-string panic payload".to_string(),
        },
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Closing the channel ends the worker's loop; wait so its files and runtime are gone.
        // There is no timeout: the join returns only when every operation already started has
        // finished (see the note on `Store`).
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            // A thread cannot join itself; with the channel closed it ends on its own.
            if !on_worker() {
                let _ = t.join();
            }
        }
    }
}

/// A [`crate::Store`] for synchronous code. Same needs, same refusals, same atomic replace.
///
/// An operation that panics is reported as [`Error::Panicked`] and the store keeps working.
///
/// Dropping a `Store` waits, with no timeout, for the worker thread to finish. Operations from other
/// threads that are still in flight run to completion first, so a drop can block as long as the
/// slowest of them (a hung backend call blocks it indefinitely). Drop a `Store` only when no
/// thread is mid-call, or accept that wait.
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
        if on_worker() {
            return Err(Error::Reentrant);
        }
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

    /// Size and modification time of `key` (see [`crate::Store::stat`]).
    pub fn stat(&self, key: &str) -> Result<Entry> {
        let (s, key) = (self.inner.clone(), key.to_string());
        self.worker.run(async move { s.stat(&key).await })
    }

    /// One page of keys under `prefix` (see [`crate::Store::list`], including its cost).
    pub fn list(&self, prefix: &str, after: Option<&str>, limit: NonZeroUsize) -> Result<Page> {
        let (s, prefix, after) = (
            self.inner.clone(),
            prefix.to_string(),
            after.map(str::to_string),
        );
        self.worker
            .run(async move { s.list(&prefix, after.as_deref(), limit).await })
    }

    /// Write only if the key is absent (see [`crate::Store::create_if_absent`]).
    pub fn create_if_absent(&self, key: &str, bytes: Vec<u8>) -> Result<()> {
        let (s, key) = (self.inner.clone(), key.to_string());
        self.worker
            .run(async move { s.create_if_absent(&key, bytes).await })
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn panics(formatted: bool) -> Result<()> {
        if formatted {
            panic!("boom {}", 7);
        }
        panic!("static boom");
    }

    #[test]
    fn a_panicking_operation_is_reported_and_the_worker_survives() {
        let w = Worker::start().unwrap();
        let err = w.run(panics(true)).unwrap_err();
        assert!(
            matches!(&err, Error::Panicked(m) if m == "boom 7"),
            "{err:?}"
        );
        let err = w.run(panics(false)).unwrap_err();
        assert!(
            matches!(&err, Error::Panicked(m) if m == "static boom"),
            "{err:?}"
        );
        assert_eq!(w.run(async { Ok(5) }).unwrap(), 5);
    }

    /// A blocking call made from a future that is itself running on the worker used to park the
    /// worker's only thread on its own reply: a deadlock. The call runs on its own thread so the
    /// test can time out instead of hanging the suite.
    #[test]
    fn a_call_from_the_workers_own_runtime_does_not_deadlock() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let w = Arc::new(Worker::start().unwrap());
            let inner = w.clone();
            let out = w.run(async move { Ok(inner.run(async { Ok(1u8) })) });
            let _ = done_tx.send(out);
        });
        let out = done_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("deadlocked: the worker waited on its own reply");
        // The outer call completes; the call made from inside it is the one refused.
        let inner = out.expect("the outer call must succeed");
        assert!(matches!(inner, Err(Error::Reentrant)), "{inner:?}");
    }
}
