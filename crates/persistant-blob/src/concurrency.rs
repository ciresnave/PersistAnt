// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime-agnostic helpers for sampled-concurrency contract tests, shared with `persistant-kv`.
//!
//! No spawn and no runtime: tasks are polled round-robin on the calling task, so interleaving is
//! at await points and real parallelism comes from the backend's own threads.

use std::future::Future;
use std::pin::Pin;
use std::task::Poll;

/// A boxed future borrowed from the test's stack.
pub type Task<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// Let other tasks polled by the same [`join_all`] run once.
pub async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// Poll every future on this task until all are done; results are in input order.
pub async fn join_all<T>(tasks: Vec<Task<'_, T>>) -> Vec<T> {
    let mut slots: Vec<Option<T>> = tasks.iter().map(|_| None).collect();
    let mut tasks: Vec<Option<Task<'_, T>>> = tasks.into_iter().map(Some).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (i, t) in tasks.iter_mut().enumerate() {
            let ready = match t.as_mut() {
                Some(fut) => match fut.as_mut().poll(cx) {
                    Poll::Ready(v) => Some(v),
                    Poll::Pending => {
                        pending = true;
                        None
                    }
                },
                None => None,
            };
            if let Some(v) = ready {
                slots[i] = Some(v);
                *t = None;
            }
        }
        if pending {
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    })
    .await;
    slots.into_iter().flatten().collect()
}
