// SPDX-License-Identifier: MIT OR Apache-2.0
//! The hello-world the `README` opens with is `examples/hello.rs` (which CI builds and runs), word
//! for word, and at most ten lines.

#[test]
fn the_readme_hello_is_the_example_file() {
    let readme = include_str!("../README.md").replace("\r\n", "\n");
    let after = readme
        .split_once("## Hello\n")
        .expect("the README has a Hello section")
        .1;
    let block = after
        .split_once("```rust,ignore\n")
        .expect("the Hello section has its rust block")
        .1
        .split_once("```")
        .expect("the block ends")
        .0;
    let example = include_str!("../examples/hello.rs").replace("\r\n", "\n");
    // The file's first line is its licence header; the rest is what the README shows.
    let shown = example.split_once('\n').unwrap().1;
    assert_eq!(block, shown);
    assert!(
        block.lines().count() <= 10,
        "{} lines",
        block.lines().count()
    );
    // Control: the comparison sees a change.
    assert_ne!(block, shown.replace("hello", "goodbye"));
}
