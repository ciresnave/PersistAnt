// SPDX-License-Identifier: MIT OR Apache-2.0
//! `easyants-card.toml` is the card the EasyAnts hub reads from this repository on every build
//! (`docs/CARD-FORMAT.md` there). These checks keep it true: the sentence fits, the example it
//! names exists, the lines it shows are at most 10 and are the whole program, and `examples/hello.rs`
//! is compiled and run by CI (`cargo run --example hello --features registry`).
use std::fs;

fn card() -> String {
    fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/easyants-card.toml")).unwrap()
}

fn field(card: &str, name: &str) -> String {
    let line = card
        .lines()
        .find(|l| l.starts_with(&format!("{name} = ")))
        .unwrap_or_else(|| panic!("no `{name}` in the card"));
    line.split_once(" = ").unwrap().1.trim().to_string()
}

#[test]
fn the_card_names_an_example_that_exists_and_shows_at_most_ten_lines() {
    let card = card();
    let path = field(&card, "example_path").trim_matches('"').to_string();
    assert!(
        path.starts_with("examples/") || path.starts_with("tests/"),
        "{path}"
    );
    let text = fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let range: Vec<usize> = field(&card, "example_lines")
        .trim_matches(['[', ']'])
        .split(',')
        .map(|n| n.trim().parse().unwrap())
        .collect();
    let (first, last) = (range[0], range[1]);
    let all: Vec<&str> = text.lines().collect();
    assert!(
        first >= 1 && first <= last && last <= all.len(),
        "{range:?} of {}",
        all.len()
    );
    let shown = &all[first - 1..last];
    assert!(shown.len() <= 10, "{} lines", shown.len());
    assert!(!shown.join("\n").contains("<script"));
    // The range is the whole program: it starts at the imports and ends at the closing brace of main.
    assert!(shown[0].starts_with("use "), "{}", shown[0]);
    assert_eq!(*shown.last().unwrap(), "}");
    assert!(
        !card.contains("tokio"),
        "the card does not need a runtime named in it"
    );
}

#[test]
fn the_card_text_fits_the_hub_rules() {
    let card = card();
    assert_eq!(field(&card, "name"), "\"PersistAnt\"");
    assert!(field(&card, "sentence").trim_matches('"').chars().count() <= 160);
    assert_eq!(field(&card, "status"), "\"ready\"");
    for url in ["install_url", "docs"] {
        assert!(field(&card, url).starts_with("\"https://"), "{url}");
    }
}
