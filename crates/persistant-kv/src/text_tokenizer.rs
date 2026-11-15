// SPDX-License-Identifier: MIT OR Apache-2.0
//! Text to terms for [`crate::text::TextIndex`], behind the cargo feature `text-index-tokenizer`
//! (`docs/DESIGN-TEXT-INDEX.md` section 7). The index itself takes terms, not text, so a consumer with
//! a tokeniser of its own does not need this and does not pull the two crates it uses.
//!
//! [`tokenize`] does, in this order:
//!
//! 1. **NFKC normalisation** (`unicode-normalization`): a composed and a decomposed `é` become one
//!    form, a full-width `ＡＢＣ１２３` becomes `ABC123`, the ligature `ﬁ` becomes `fi`.
//! 2. **Lower-casing** with Unicode's default mapping (`str::to_lowercase`). This is lower-casing, not
//!    case folding: `Straße` stays `straße` and does not meet `STRASSE`.
//! 3. **Word boundaries** by UAX #29 (`unicode-segmentation`, `unicode_words`): punctuation and white
//!    space separate words and are dropped; an apostrophe inside a word and a point inside a number
//!    stay (`don't`, `3.14`); each Han ideograph is a word of its own; a run of kana is one word.
//! 4. A word longer than 64 bytes is dropped (it is a URL or an accident, not a term).
//!
//! No stemming, no stop words, no language detection. The result is sorted by term, each with how many
//! times it occurred: exactly the `&[(String, u32)]` that [`crate::text::TextIndex::put`] takes.

use std::collections::BTreeMap;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// The longest term kept, in bytes.
pub const MAX_TERM_BYTES: usize = 64;

/// The terms of `text` with their frequencies, ascending by term (see the module docs for the rules).
pub fn tokenize(text: &str) -> Vec<(String, u32)> {
    let folded: String = text.nfkc().collect::<String>().to_lowercase();
    let mut counts: BTreeMap<String, u32> = BTreeMap::new();
    for word in folded.unicode_words() {
        if word.len() <= MAX_TERM_BYTES {
            let c = counts.entry(word.to_string()).or_insert(0);
            *c = c.saturating_add(1);
        }
    }
    counts.into_iter().collect()
}
