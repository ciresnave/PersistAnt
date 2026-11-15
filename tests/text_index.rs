// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant::kv::text::TextIndex` over the memory kv: postings, document lengths and corpus
//! totals agree with a plain map held by the test (a different route from the keys) after every
//! operation of a scripted sequence of puts, replaces and removals; `Any` and `All` agree with the
//! same oracle; a replace leaves no stale posting; a removal leaves no key; concurrent writers keep
//! the totals exact; a blob store is refused.
#![cfg(feature = "text-index")]

use persistant::kv::text::{Match, TextIndex};
use persistant::kv::{KvBackend, KvError, MemoryKv};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

type Terms = BTreeMap<String, u32>;
type Oracle = BTreeMap<String, Terms>;

const VOCAB: [&str; 7] = ["alpha", "beta", "gamma", "delta", "a", "ab", "abc"];

fn pairs(t: &Terms) -> Vec<(String, u32)> {
    t.iter().map(|(k, v)| (k.clone(), *v)).collect()
}

/// A tiny deterministic generator, so a failure reproduces.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn random_terms(g: &mut Lcg) -> Terms {
    let mut t = Terms::new();
    for _ in 0..1 + g.below(4) {
        *t.entry(VOCAB[g.below(VOCAB.len())].to_string())
            .or_insert(0) += 1 + g.below(3) as u32;
    }
    t
}

fn len_of(t: &Terms) -> u32 {
    t.values().sum()
}

/// Every observable of the index against the oracle.
async fn agree(ix: &TextIndex<'_, MemoryKv>, name: &str, oracle: &Oracle, what: &str) {
    let stats = ix.stats(name).await.unwrap();
    assert_eq!(stats.docs, oracle.len() as u64, "{what}: documents");
    assert_eq!(
        stats.total_len,
        oracle.values().map(|t| u64::from(len_of(t))).sum::<u64>(),
        "{what}: total length"
    );
    for (doc, terms) in oracle {
        assert_eq!(
            ix.doc_len(name, doc).await.unwrap(),
            Some(len_of(terms)),
            "{what}: {doc}"
        );
    }
    for term in VOCAB {
        let want: Vec<(String, u32)> = oracle
            .iter()
            .filter_map(|(d, t)| t.get(term).map(|tf| (d.clone(), *tf)))
            .collect();
        // Page by two, to exercise the cursor.
        let (mut got, mut after) = (Vec::new(), None::<String>);
        loop {
            let page = ix.postings(name, term, after.as_deref(), 2).await.unwrap();
            got.extend(page.entries);
            match page.next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
        assert_eq!(got, want, "{what}: postings of {term}");
    }
    let (mut ids, mut after) = (Vec::new(), None::<String>);
    loop {
        let page = ix.docs(name, after.as_deref(), 3).await.unwrap();
        ids.extend(page.ids);
        match page.next {
            Some(c) => after = Some(c),
            None => break,
        }
    }
    assert_eq!(
        ids,
        oracle.keys().cloned().collect::<Vec<_>>(),
        "{what}: docs"
    );
    for query in [
        vec!["alpha"],
        vec!["alpha", "beta"],
        vec!["a", "abc", "gamma"],
        vec!["delta"],
    ] {
        let q: Vec<String> = query.iter().map(|s| s.to_string()).collect();
        for mode in [Match::Any, Match::All] {
            let mut want: Vec<(String, u64)> = oracle
                .iter()
                .filter_map(|(d, t)| {
                    let held: Vec<u32> = q.iter().filter_map(|w| t.get(w).copied()).collect();
                    let ok = match mode {
                        Match::Any => !held.is_empty(),
                        Match::All => held.len() == q.len(),
                    };
                    ok.then(|| (d.clone(), held.iter().map(|x| u64::from(*x)).sum()))
                })
                .collect();
            want.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            let got: Vec<(String, u64)> = ix
                .search(name, &q, mode, 100)
                .await
                .unwrap()
                .into_iter()
                .map(|h| (h.doc_id, h.score))
                .collect();
            assert_eq!(got, want, "{what}: {mode:?} {q:?}");
        }
    }
}

/// No key of the index names a posting outside the oracle: a replace or a removal leaves none.
async fn no_stale_keys(kv: &MemoryKv, name: &str, oracle: &Oracle) {
    let mut want: BTreeSet<String> = BTreeSet::new();
    for (d, terms) in oracle {
        for t in terms.keys() {
            want.insert(format!("tx\u{1f}{name}\u{1f}{t}\u{1f}{d}"));
        }
        want.insert(format!("td\u{1f}{name}\u{1f}{d}"));
    }
    if !oracle.is_empty() {
        want.insert(format!("tt\u{1f}{name}"));
    }
    let mut have: BTreeSet<String> = BTreeSet::new();
    for prefix in ["tx\u{1f}", "td\u{1f}", "tt\u{1f}"] {
        let mut after = None::<String>;
        loop {
            let page = kv.ordered_scan(prefix, after.as_deref(), 50).await.unwrap();
            have.extend(page.keys);
            match page.next {
                Some(c) => after = Some(c),
                None => break,
            }
        }
    }
    assert_eq!(have, want, "the keys under the index prefixes");
}

#[tokio::test]
async fn a_scripted_sequence_agrees_with_the_oracle_after_every_operation() {
    let kv = MemoryKv::new();
    let ix = TextIndex::new(&kv);
    let mut oracle = Oracle::new();
    let mut g = Lcg(7);
    let docs = ["d1", "d2", "d3", "d4", "d5"];
    agree(&ix, "i", &oracle, "empty").await;
    for step in 0..120 {
        let doc = docs[g.below(docs.len())];
        if g.below(4) == 0 {
            let removed = ix.remove_many("i", &[doc]).await.unwrap();
            assert_eq!(
                removed,
                usize::from(oracle.remove(doc).is_some()),
                "step {step}"
            );
        } else {
            let t = random_terms(&mut g);
            ix.put("i", doc, &pairs(&t)).await.unwrap();
            oracle.insert(doc.to_string(), t);
        }
        let what = format!("step {step}");
        agree(&ix, "i", &oracle, &what).await;
        no_stale_keys(&kv, "i", &oracle).await;
    }
    // Several at once, and one that is not there.
    ix.remove_many("i", &["d1", "d2", "nope"]).await.unwrap();
    oracle.remove("d1");
    oracle.remove("d2");
    agree(&ix, "i", &oracle, "bulk remove").await;
    no_stale_keys(&kv, "i", &oracle).await;
    let all: Vec<&str> = docs.to_vec();
    ix.remove_many("i", &all).await.unwrap();
    oracle.clear();
    agree(&ix, "i", &oracle, "everything removed").await;
    no_stale_keys(&kv, "i", &oracle).await;
}

#[tokio::test]
async fn two_indexes_do_not_see_each_other() {
    let kv = MemoryKv::new();
    let ix = TextIndex::new(&kv);
    ix.put("one", "d", &[("alpha".into(), 2)]).await.unwrap();
    ix.put("two", "d", &[("beta".into(), 5)]).await.unwrap();
    assert_eq!(ix.stats("one").await.unwrap().total_len, 2);
    assert_eq!(ix.stats("two").await.unwrap().total_len, 5);
    assert!(
        ix.postings("one", "beta", None, 10)
            .await
            .unwrap()
            .entries
            .is_empty()
    );
    ix.remove_many("one", &["d"]).await.unwrap();
    assert_eq!(ix.doc_len("two", "d").await.unwrap(), Some(5));
}

#[tokio::test]
async fn equal_terms_add_up_and_bad_input_is_refused() {
    let kv = MemoryKv::new();
    let ix = TextIndex::new(&kv);
    ix.put(
        "i",
        "d",
        &[("a".into(), 1), ("a".into(), 2), ("b".into(), 4)],
    )
    .await
    .unwrap();
    assert_eq!(ix.doc_len("i", "d").await.unwrap(), Some(7));
    assert_eq!(
        ix.postings("i", "a", None, 10).await.unwrap().entries,
        [("d".to_string(), 3)]
    );
    for bad in [
        ix.put("", "d", &[]).await,
        ix.put("i", "", &[]).await,
        ix.put("i\u{1f}x", "d", &[]).await,
        ix.put("i", "d\u{1f}", &[]).await,
        ix.put("i", "d", &[("t\u{1f}".into(), 1)]).await,
        ix.put("i", "d", &[("".into(), 1)]).await,
        ix.put("i", "d", &[("t".into(), 0)]).await,
        ix.put("i", "d", &[("x".repeat(513), 1)]).await,
        ix.put("i", "d", &[("t".into(), u32::MAX), ("t".into(), 1)])
            .await,
    ] {
        assert!(matches!(bad, Err(KvError::Backend(_))), "{bad:?}");
    }
    // Nothing the refused calls did is visible.
    assert_eq!(ix.stats("i").await.unwrap().docs, 1);
    // A document with no terms is a document of length zero.
    ix.put("i", "empty", &[]).await.unwrap();
    assert_eq!(ix.doc_len("i", "empty").await.unwrap(), Some(0));
    assert_eq!(ix.stats("i").await.unwrap().docs, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writers_keep_the_totals_exact() {
    let kv = Arc::new(MemoryKv::new());
    // Different documents: every one counted, and the length is the sum.
    let tasks: Vec<_> = (0..12)
        .map(|i| {
            let kv = kv.clone();
            tokio::spawn(async move {
                TextIndex::new(&*kv)
                    .put("i", &format!("d{i:02}"), &[("alpha".into(), 1 + i as u32)])
                    .await
                    .unwrap();
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let ix = TextIndex::new(&*kv);
    let stats = ix.stats("i").await.unwrap();
    assert_eq!(stats.docs, 12);
    assert_eq!(stats.total_len, (1..=12).sum::<u64>());
    assert_eq!(
        ix.postings("i", "alpha", None, 100)
            .await
            .unwrap()
            .entries
            .len(),
        12
    );

    // The same document, eight different texts at once: the postings are those of exactly one.
    let texts: Vec<String> = (0..8).map(|i| format!("w{i}")).collect();
    let tasks: Vec<_> = texts
        .iter()
        .cloned()
        .map(|w| {
            let kv = kv.clone();
            tokio::spawn(async move {
                TextIndex::new(&*kv)
                    .put("j", "same", &[(w, 3)])
                    .await
                    .unwrap();
            })
        })
        .collect();
    for t in tasks {
        t.await.unwrap();
    }
    let ix = TextIndex::new(&*kv);
    let mut holders = Vec::new();
    for w in &texts {
        if !ix
            .postings("j", w, None, 10)
            .await
            .unwrap()
            .entries
            .is_empty()
        {
            holders.push(w.clone());
        }
    }
    assert_eq!(
        holders.len(),
        1,
        "postings of more than one text survived: {holders:?}"
    );
    let stats = ix.stats("j").await.unwrap();
    assert_eq!((stats.docs, stats.total_len), (1, 3));
}

#[tokio::test]
async fn a_blob_store_is_refused_and_a_corrupt_record_is_reported() {
    let kv = persistant::kv::KvOnBlob::new(persistant::blob::MemoryBlob::new());
    let ix = TextIndex::new(&kv);
    assert!(matches!(
        ix.put("i", "d", &[("a".into(), 1)]).await,
        Err(KvError::Unsupported(_))
    ));

    let kv = MemoryKv::new();
    kv.put("td\u{1f}i\u{1f}d", b"\x00\x00".to_vec(), None)
        .await
        .unwrap();
    let ix = TextIndex::new(&kv);
    assert!(matches!(
        ix.doc_len("i", "d").await,
        Err(KvError::Corrupt(_))
    ));
    kv.put("tt\u{1f}i", b"short".to_vec(), None).await.unwrap();
    assert!(matches!(ix.stats("i").await, Err(KvError::Corrupt(_))));
}
