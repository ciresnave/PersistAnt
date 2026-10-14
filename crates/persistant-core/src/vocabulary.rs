// SPDX-License-Identifier: MIT OR Apache-2.0
//! The seed vocabulary: the ids of `docs/DESIGN-KINDS.md` §2.2 that are not `rel.*` constructs
//! (those are UniSQL's leaf ids), each with its written contract.
//!
//! Nothing here is *on* for any backend. An id claims nothing until a row in a matrix says a test
//! passed (§2.1). The list is a seed, not the boundary: ids are added by feature-space coverage,
//! never changed in meaning, and a contract here is the sentence a test must satisfy.

use crate::CapabilityId;

/// `(id, contract)`.
pub const SEED: &[(&str, &str)] = &[
    (
        "blob.read",
        "get returns exactly the bytes last written under the key, or reports the key absent",
    ),
    (
        "blob.write",
        "put stores the bytes under the key; a later read returns them",
    ),
    (
        "blob.delete",
        "delete removes the key; a later read reports it absent; deleting an absent key is not an error",
    ),
    (
        "blob.stat",
        "stat returns the length of the value under the key without reading it, or reports it absent",
    ),
    (
        "blob.list",
        "list returns every key with a literal prefix in ascending bytewise order, in pages with a continuation",
    ),
    (
        "blob.replace_atomic",
        "a concurrent reader sees the old value or the new value, never a mix, and a crash leaves one of them",
    ),
    (
        "blob.replace_atomic.concurrent_read",
        "a concurrent reader sees the whole old value or the whole new value, never a mix of the two",
    ),
    (
        "blob.create_exclusive",
        "a write that names itself exclusive succeeds for exactly one of any number of concurrent writers",
    ),
    (
        "blob.create_exclusive_atomic",
        "as blob.create_exclusive, and the content is fully visible the moment the key exists",
    ),
    (
        "blob.durable",
        "an acknowledged write, and its parent directory entry, survive loss of power",
    ),
    (
        "blob.file_mode",
        "a value created with an owner-only mode is not readable by another user",
    ),
    (
        "blob.append",
        "appended bytes are added after the existing bytes in call order",
    ),
    (
        "blob.append.concurrent_whole",
        "of concurrent appends to one key, every call's bytes appear whole and exactly once, none interleaved with another's",
    ),
    (
        "blob.max_value_size",
        "one value of the stated size is stored and returned intact",
    ),
    (
        "kv.get",
        "get returns the value last put under the key, or reports it absent",
    ),
    (
        "kv.put",
        "put stores the value under the key, replacing any earlier value",
    ),
    (
        "kv.delete",
        "delete removes the key and a later get reports it absent",
    ),
    (
        "kv.ttl",
        "an entry written with a time to live is invisible to get and to listing once it has expired",
    ),
    (
        "kv.ttl_remaining",
        "the remaining time to live of a live entry can be read back",
    ),
    (
        "kv.put_if_absent",
        "of any number of concurrent put_if_absent calls for one key, exactly one succeeds",
    ),
    (
        "kv.compare_and_swap",
        "an update applies only if the value is still the one the caller read",
    ),
    (
        "kv.compare_and_delete",
        "a live value equal to the expected one is removed for exactly one of any number of concurrent callers; any other value is left alone",
    ),
    (
        "kv.take",
        "get-and-delete returns the value to exactly one of any number of concurrent callers",
    ),
    (
        "kv.batch_atomic",
        "several keys written in one call are all visible or none is, to every reader, including after a crash",
    ),
    (
        "kv.ordered_scan",
        "a range or prefix scan returns keys in ascending bytewise order, in pages, each key once",
    ),
    (
        "kv.prefix_list",
        "a prefix listing returns every key present throughout at least once, bounded per page, order unspecified, expired entries never",
    ),
    (
        "kv.prefix_list.exactly_once",
        "as kv.prefix_list, and no key is returned twice",
    ),
    (
        "kv.count",
        "the number of live keys under a prefix is returned",
    ),
    (
        "lock.lease",
        "an exclusive lease has an owner token and an expiry and is safely reclaimed after it",
    ),
    (
        "lock.process",
        "a lock is held until the holding process dies, with no staleness",
    ),
    (
        "storage.no_plaintext_at_rest",
        "a canary value written through the layer is not found as raw bytes in the data directory, log or temp files",
    ),
    (
        "ops.pitr",
        "the data can be restored to a chosen instant and equals a snapshot taken at that instant",
    ),
    (
        "rel.source_dialect",
        "statements written in the named dialect are accepted and translated for this backend",
    ),
];

/// The seed as validated ids. Panics only if [`SEED`] has an invalid id, which a test forbids.
pub fn seed() -> Vec<(CapabilityId, &'static str)> {
    SEED.iter()
        .map(|(id, contract)| (CapabilityId::new(*id).expect("seed id is valid"), *contract))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_seed_id_is_valid_unique_and_has_a_contract() {
        let mut seen = BTreeSet::new();
        for (id, contract) in SEED {
            assert!(CapabilityId::new(*id).is_ok(), "{id}");
            assert!(seen.insert(*id), "duplicate {id}");
            assert!(
                contract.split_whitespace().count() >= 5,
                "{id}: contract too thin"
            );
        }
        assert_eq!(seed().len(), SEED.len());
    }

    #[test]
    fn carries_what_the_mailbox_contract_needs() {
        // Synapse's MailStore (PM notice 2026-10-14): atomic multi-key transaction, ordered
        // composite-key scan, no-clobber atomic publish, 0600 file mode, OS file lock.
        for want in [
            "kv.batch_atomic",
            "kv.ordered_scan",
            "blob.create_exclusive_atomic",
            "blob.file_mode",
            "lock.process",
        ] {
            assert!(
                SEED.iter().any(|(id, _)| *id == want),
                "control: {want} must be present"
            );
        }
        assert!(
            !SEED.iter().any(|(id, _)| *id == "mailbox.redb"),
            "control: the check can fail"
        );
    }
}
