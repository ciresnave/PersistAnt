// SPDX-License-Identifier: MIT OR Apache-2.0
//! `update_record`: a typed read-modify-write on a key-value backend (ROADMAP row 41, step 1).
//!
//! The record is stored in the same envelope as [`Store`](crate::Store) uses (`kind`, `schema`,
//! `data`), so an older schema is upgraded on read and a record of another kind is refused. The
//! write is `persistant_kv::cas::update`: compare-and-swap on the stored bytes, retried when another
//! caller won, bounded by [`persistant_kv::cas::ATTEMPTS`]. It is on exactly where `kv.compare_and_swap`
//! is on (SQLite, PostgreSQL, MySQL, MariaDB and the in-process memory backend) and refused with
//! `KvError::Unsupported` elsewhere, a blob store included.

use crate::{Error, Record, store};
use persistant_kv::{KvBackend, KvError, cas};
use std::fmt;
use std::time::Duration;

/// Why [`update_record`] did not commit.
#[derive(Debug)]
pub enum RecordUpdateError {
    /// The stored value is not a readable record of this type, or the new one could not be encoded.
    Record(Error),
    /// The backend failed or cannot compare-and-swap (`KvError::Unsupported`).
    Kv(KvError),
    /// Another caller changed the key `attempts` times in a row; nothing was written.
    Contended {
        /// How many reads and writes were tried.
        attempts: usize,
    },
}

impl fmt::Display for RecordUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordUpdateError::Record(e) => write!(f, "{e}"),
            RecordUpdateError::Kv(e) => write!(f, "{e}"),
            RecordUpdateError::Contended { attempts } => {
                write!(
                    f,
                    "lost the race {attempts} times in a row; the key is too hot"
                )
            }
        }
    }
}

impl std::error::Error for RecordUpdateError {}

/// Replace the record at `key` with `f(current)`, atomically against concurrent writers.
///
/// `f` gets the current record (`None`: the key is absent or expired) and returns the new one, or
/// `None` to leave the key alone. `Ok(true)`: the new record was committed; `Ok(false)`: `f`
/// declined. `f` runs again after each lost race, so it must be a pure function of its argument.
pub async fn update_record<K, R, F>(
    kv: &K,
    key: &str,
    ttl: Option<Duration>,
    mut f: F,
) -> Result<bool, RecordUpdateError>
where
    K: KvBackend,
    R: Record,
    F: FnMut(Option<R>) -> Option<R>,
{
    let mut failed: Option<Error> = None;
    let out = cas::update(kv, key, ttl, |old| {
        failed = None;
        let current = match old {
            None => None,
            Some(bytes) => match store::decode_record::<R>(bytes) {
                Ok(r) => Some(r),
                Err(e) => {
                    failed = Some(e);
                    return None;
                }
            },
        };
        let next = f(current)?;
        match store::encode_record(&next) {
            Ok(b) => Some(b),
            Err(e) => {
                failed = Some(e);
                None
            }
        }
    })
    .await;
    if let Some(e) = failed {
        return Err(RecordUpdateError::Record(e));
    }
    match out {
        Ok(committed) => Ok(committed.is_some()),
        Err(cas::UpdateError::Kv(e)) => Err(RecordUpdateError::Kv(e)),
        Err(cas::UpdateError::Contended { attempts }) => {
            Err(RecordUpdateError::Contended { attempts })
        }
    }
}
