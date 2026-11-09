// SPDX-License-Identifier: MIT OR Apache-2.0
//! `kv.rate_window`: a sliding-window limit and a token bucket as a layer over any [`KvBackend`]
//! (ROADMAP row 27; the Redis ZSET and HASH patterns of auth-framework's rate limiter, without Redis).
//!
//! Each limiter keeps **one key**. A decision reads it, computes the next state and writes it with
//! `compare_and_swap` (`put_if_absent` for the first write), retrying when another caller won the
//! race, so of any number of concurrent callers racing for the last slot exactly one is allowed. A
//! refusal writes nothing: being limited does not extend the limit. The key carries a time to live
//! (the window; the time to refill a bucket), so an idle limiter disappears by itself.
//!
//! The layer needs `kv.put_if_absent` and `kv.compare_and_swap` **on** for the backend (their tests
//! say so, not this module). On a backend without them a second call returns
//! [`KvError::Unsupported`]; a limit is not approximated. In particular [`crate::KvOnBlob`] cannot
//! offer it. The conformance leaf is `kv.rate_window`.
//!
//! Time is milliseconds since the Unix epoch, read from the caller's clock: the `_at` methods take it
//! as an argument (tests and replays), the others read the system clock. **Every caller of one key
//! must use clocks that agree**; a limiter shared by machines is as accurate as their clocks.

use crate::{KvBackend, KvError};
use persistant_blob::concurrency::yield_now;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How often a decision retries after losing a race before it reports contention.
const ATTEMPTS: usize = 64;

/// The answer to one request against a limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The request fits; `remaining` more would fit right now.
    Allowed {
        /// Requests still available.
        remaining: u32,
    },
    /// The request does not fit; nothing was recorded. It would fit after `retry_after` (assuming no
    /// one else uses the slot first).
    Denied {
        /// How long until a slot frees.
        retry_after: Duration,
    },
}

impl Decision {
    /// Whether the request was allowed.
    pub fn is_allowed(self) -> bool {
        matches!(self, Decision::Allowed { .. })
    }
}

/// Rate limiters on a borrowed [`KvBackend`].
#[derive(Debug)]
pub struct RateWindow<'a, K> {
    kv: &'a K,
}

impl<K> Clone for RateWindow<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for RateWindow<'_, K> {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn corrupt(what: &str) -> KvError {
    KvError::Corrupt(format!("rate_window: {what}"))
}

fn contended() -> KvError {
    KvError::Backend(format!(
        "rate_window: lost the race {ATTEMPTS} times in a row; the key is too hot for one limiter"
    ))
}

impl<'a, K: KvBackend> RateWindow<'a, K> {
    /// Limiters on `kv`.
    pub fn new(kv: &'a K) -> Self {
        Self { kv }
    }

    /// Write `next` over what `current` was read as (`None`: the key was absent). `Ok(false)`: lost.
    async fn commit(
        &self,
        key: &str,
        current: Option<&[u8]>,
        next: Vec<u8>,
        ttl: Duration,
    ) -> Result<bool, KvError> {
        match current {
            None => self.kv.put_if_absent(key, next, Some(ttl)).await,
            Some(old) => self.kv.compare_and_swap(key, old, next, Some(ttl)).await,
        }
    }

    /// Sliding window: at most `limit` allowed requests in any span of `window`, now.
    pub async fn sliding_window(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<Decision, KvError> {
        self.sliding_window_at(key, limit, window, now_ms()).await
    }

    /// [`sliding_window`](Self::sliding_window) at `now` (milliseconds since the epoch). A request
    /// recorded at `t` counts until `t + window`.
    pub async fn sliding_window_at(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
        now: u64,
    ) -> Result<Decision, KvError> {
        let window_ms = window.as_millis() as u64;
        for _ in 0..ATTEMPTS {
            let current = self.kv.get(key).await?;
            let mut events = match &current {
                None => Vec::new(),
                Some(raw) => parse_events(raw)?,
            };
            events.retain(|&t| now.saturating_sub(t) < window_ms);
            if events.len() >= limit as usize {
                let retry = events.first().map_or(window_ms, |&oldest| {
                    (oldest + window_ms).saturating_sub(now)
                });
                return Ok(Decision::Denied {
                    retry_after: Duration::from_millis(retry),
                });
            }
            let remaining = limit - events.len() as u32 - 1;
            events.push(now);
            // Let other callers run between the read and the write: the race is real on any backend
            // with a network or a lock in it, and this is how a test sees it on one without.
            yield_now().await;
            if self
                .commit(key, current.as_deref(), encode_events(&events), window)
                .await?
            {
                return Ok(Decision::Allowed { remaining });
            }
        }
        Err(contended())
    }

    /// Token bucket: `capacity` tokens, one more every `refill` up to `capacity`; a request takes
    /// one. A new bucket starts full.
    pub async fn token_bucket(
        &self,
        key: &str,
        capacity: u32,
        refill: Duration,
    ) -> Result<Decision, KvError> {
        self.token_bucket_at(key, capacity, refill, now_ms()).await
    }

    /// [`token_bucket`](Self::token_bucket) at `now` (milliseconds since the epoch).
    pub async fn token_bucket_at(
        &self,
        key: &str,
        capacity: u32,
        refill: Duration,
        now: u64,
    ) -> Result<Decision, KvError> {
        let step = refill.as_millis() as u64;
        if step == 0 {
            return Err(KvError::Backend(
                "rate_window: the refill interval must be at least one millisecond".to_string(),
            ));
        }
        // Idle for this long and the bucket is full again, the same as an absent one.
        let ttl = refill.saturating_mul(capacity.max(1));
        for _ in 0..ATTEMPTS {
            let current = self.kv.get(key).await?;
            let (mut tokens, mut last) = match &current {
                None => (u64::from(capacity), now),
                Some(raw) => parse_bucket(raw)?,
            };
            let gained = now.saturating_sub(last) / step;
            if tokens + gained >= u64::from(capacity) {
                tokens = u64::from(capacity);
                last = now;
            } else {
                tokens += gained;
                last += gained * step;
            }
            if tokens == 0 {
                let waited = now.saturating_sub(last);
                return Ok(Decision::Denied {
                    retry_after: Duration::from_millis(step.saturating_sub(waited)),
                });
            }
            tokens -= 1;
            yield_now().await;
            if self
                .commit(key, current.as_deref(), encode_bucket(tokens, last), ttl)
                .await?
            {
                return Ok(Decision::Allowed {
                    remaining: tokens as u32,
                });
            }
        }
        Err(contended())
    }
}

fn encode_events(events: &[u64]) -> Vec<u8> {
    events
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
        .into_bytes()
}

fn parse_events(raw: &[u8]) -> Result<Vec<u64>, KvError> {
    let text = std::str::from_utf8(raw).map_err(|_| corrupt("a window is not text"))?;
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let mut events = text
        .split(',')
        .map(|t| {
            t.parse::<u64>()
                .map_err(|_| corrupt("a window holds a non-number"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    events.sort_unstable();
    Ok(events)
}

fn encode_bucket(tokens: u64, last: u64) -> Vec<u8> {
    format!("{tokens},{last}").into_bytes()
}

fn parse_bucket(raw: &[u8]) -> Result<(u64, u64), KvError> {
    let text = std::str::from_utf8(raw).map_err(|_| corrupt("a bucket is not text"))?;
    let (tokens, last) = text
        .split_once(',')
        .ok_or_else(|| corrupt("a bucket is not `tokens,last`"))?;
    let num = |s: &str| {
        s.parse::<u64>()
            .map_err(|_| corrupt("a bucket holds a non-number"))
    };
    Ok((num(tokens)?, num(last)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKv;

    #[tokio::test]
    async fn sliding_window_counts_recent_requests_and_frees_slots_as_they_age() {
        let kv = MemoryKv::new();
        let rw = RateWindow::new(&kv);
        let w = Duration::from_secs(10);
        for (i, want) in [2, 1, 0].into_iter().enumerate() {
            let d = rw
                .sliding_window_at("k", 3, w, 1_000 + i as u64)
                .await
                .unwrap();
            assert_eq!(d, Decision::Allowed { remaining: want });
        }
        // Full: the oldest request (t=1000) frees at 11000.
        let d = rw.sliding_window_at("k", 3, w, 5_000).await.unwrap();
        assert_eq!(
            d,
            Decision::Denied {
                retry_after: Duration::from_millis(6_000)
            }
        );
        // A refusal recorded nothing: the same retry hint a moment later.
        let d = rw.sliding_window_at("k", 3, w, 5_001).await.unwrap();
        assert_eq!(
            d,
            Decision::Denied {
                retry_after: Duration::from_millis(5_999)
            }
        );
        // The request at t=1000 aged out at 11000, so one slot is free; the next frees at 11001.
        assert_eq!(
            rw.sliding_window_at("k", 3, w, 11_000).await.unwrap(),
            Decision::Allowed { remaining: 0 }
        );
        assert_eq!(
            rw.sliding_window_at("k", 3, w, 11_000).await.unwrap(),
            Decision::Denied {
                retry_after: Duration::from_millis(1)
            }
        );
    }

    #[tokio::test]
    async fn token_bucket_refills_one_token_per_interval_up_to_capacity() {
        let kv = MemoryKv::new();
        let rw = RateWindow::new(&kv);
        let step = Duration::from_millis(1000);
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 0).await.unwrap(),
            Decision::Allowed { remaining: 1 }
        );
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 0).await.unwrap(),
            Decision::Allowed { remaining: 0 }
        );
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 0).await.unwrap(),
            Decision::Denied {
                retry_after: Duration::from_millis(1000)
            }
        );
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 1000).await.unwrap(),
            Decision::Allowed { remaining: 0 }
        );
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 1500).await.unwrap(),
            Decision::Denied {
                retry_after: Duration::from_millis(500)
            }
        );
        // A long idle refills to capacity, not beyond.
        assert_eq!(
            rw.token_bucket_at("b", 2, step, 99_000).await.unwrap(),
            Decision::Allowed { remaining: 1 }
        );
    }

    #[tokio::test]
    async fn a_zero_refill_interval_is_refused() {
        let kv = MemoryKv::new();
        assert!(
            RateWindow::new(&kv)
                .token_bucket_at("b", 1, Duration::ZERO, 0)
                .await
                .is_err()
        );
    }
}
