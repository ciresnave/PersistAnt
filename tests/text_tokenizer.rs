// SPDX-License-Identifier: MIT OR Apache-2.0
//! `persistant::kv::text_tokenizer::tokenize` on a fixed corpus (each case states the rule it checks),
//! and end to end: text tokenised, put into a `TextIndex`, found again by a differently written query.
#![cfg(feature = "text-index-tokenizer")]

use persistant::kv::MemoryKv;
use persistant::kv::text::{Match, TextIndex};
use persistant::kv::text_tokenizer::{MAX_TERM_BYTES, tokenize};

fn t(text: &str) -> Vec<(String, u32)> {
    tokenize(text)
}

fn pairs(v: &[(&str, u32)]) -> Vec<(String, u32)> {
    v.iter().map(|(a, b)| (a.to_string(), *b)).collect()
}

#[test]
fn the_fixed_corpus() {
    // Words, case and counts: sorted by term, frequencies added up.
    assert_eq!(
        t("The cat and the Cat; THE end."),
        pairs(&[("and", 1), ("cat", 2), ("end", 1), ("the", 3)])
    );
    // Punctuation and white space separate words and are dropped; nothing is nothing.
    assert_eq!(t(""), Vec::new());
    assert_eq!(t("  \t\n -- ?! ... "), Vec::new());
    // An apostrophe inside a word and a point inside a number stay (UAX #29).
    assert_eq!(
        t("don't stop 3.14 times"),
        pairs(&[("3.14", 1), ("don't", 1), ("stop", 1), ("times", 1)])
    );
    // Digits are words; underscores and hyphens split or join per UAX #29, so state what happens.
    assert_eq!(t("a1 b2 a1"), pairs(&[("a1", 2), ("b2", 1)]));
    // Short tokens are kept (one and two characters).
    assert_eq!(t("a I go"), pairs(&[("a", 1), ("go", 1), ("i", 1)]));
}

#[test]
fn normalisation_makes_equal_things_equal() {
    // Composed and decomposed e-acute are one term.
    assert_eq!(t("caf\u{e9}"), t("cafe\u{301}"));
    assert_eq!(t("caf\u{e9}"), pairs(&[("caf\u{e9}", 1)]));
    // Full-width forms and the fi ligature fold to their plain forms (NFKC).
    assert_eq!(
        t("\u{ff21}\u{ff22}\u{ff23}\u{ff11}\u{ff12}\u{ff13}"),
        pairs(&[("abc123", 1)])
    );
    assert_eq!(t("\u{fb01}sh"), pairs(&[("fish", 1)]));
    // Lower-casing is not case folding: this is stated, not hidden.
    assert_eq!(t("Stra\u{df}e"), pairs(&[("stra\u{df}e", 1)]));
    assert_ne!(t("Stra\u{df}e"), t("STRASSE"));
}

#[test]
fn scripts_without_spaces() {
    // Each Han ideograph is a word of its own (UAX #29).
    assert_eq!(
        t("\u{4f60}\u{597d}\u{4e16}\u{754c}"),
        pairs(&[
            ("\u{4e16}", 1),
            ("\u{4f60}", 1),
            ("\u{597d}", 1),
            ("\u{754c}", 1)
        ])
    );
    // A run of katakana is one word.
    assert_eq!(
        t("\u{30ab}\u{30bf}\u{30ab}\u{30ca}"),
        pairs(&[("\u{30ab}\u{30bf}\u{30ab}\u{30ca}", 1)])
    );
    // Mixed scripts split where the script changes.
    assert_eq!(
        t("rust\u{8a00}\u{8a9e}"),
        pairs(&[("rust", 1), ("\u{8a00}", 1), ("\u{8a9e}", 1)])
    );
}

#[test]
fn an_overlong_word_is_dropped_and_the_limit_is_in_bytes() {
    // The limit is written out here, not read from the constant, so moving the constant fails this.
    assert_eq!(MAX_TERM_BYTES, 64);
    let at_limit = "x".repeat(64);
    let over = "x".repeat(65);
    assert_eq!(
        t(&format!("{at_limit} {over} ok")),
        pairs(&[("ok", 1), (&at_limit, 1)])
    );
    // 33 two-byte characters are 66 bytes: over the limit though only 33 characters.
    let wide = "\u{e9}".repeat(33);
    assert_eq!(t(&format!("{wide} ok")), pairs(&[("ok", 1)]));
    // 32 two-byte characters are 64 bytes: kept.
    let narrow = "\u{e9}".repeat(32);
    assert_eq!(t(&narrow), pairs(&[(&narrow, 1)]));
}

#[tokio::test]
async fn tokenised_text_is_found_by_a_differently_written_query() {
    let kv = MemoryKv::new();
    let ix = TextIndex::new(&kv);
    ix.put("i", "d1", &tokenize("Caf\u{e9} au lait, caf\u{e9}!"))
        .await
        .unwrap();
    ix.put("i", "d2", &tokenize("The cafe\u{301} is closed"))
        .await
        .unwrap();
    ix.put("i", "d3", &tokenize("nothing here")).await.unwrap();
    // Queries are tokenised the same way: composed, decomposed or shouted, all meet.
    for q in ["caf\u{e9}", "CAFE\u{301}", "\u{ff23}\u{ff21}\u{ff26}\u{c9}"] {
        let terms: Vec<String> = tokenize(q).into_iter().map(|(w, _)| w).collect();
        let hits = ix.search("i", &terms, Match::Any, 10).await.unwrap();
        let got: Vec<(&str, u64)> = hits.iter().map(|h| (h.doc_id.as_str(), h.score)).collect();
        assert_eq!(got, [("d1", 2), ("d2", 1)], "query {q:?}");
    }
    assert!(
        ix.search(
            "i",
            &["lait".to_string(), "closed".to_string()],
            Match::All,
            10
        )
        .await
        .unwrap()
        .is_empty()
    );
}
