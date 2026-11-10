// SPDX-License-Identifier: MIT OR Apache-2.0
//! A consumer that uses `chrono` must keep `sqlx::query!` returning chrono types.
//!
//! sqlx's macros return `time` types as soon as sqlx's `time` feature is on anywhere in the
//! build, and cargo unifies features across the graph. 0.16.8 enabled `time` here, which turned
//! every `query!` site of a chrono consumer (Humboldt: 39 of them) into a compile error. This
//! reads the resolved feature set of sqlx for what a consumer builds (normal and build edges, no
//! dev-dependencies, which enable `chrono` for the tests only) and refuses `time`.
//!
//! Mutation: put `"time"` back in the `sqlx` features of this crate's `Cargo.toml` and both tests
//! fail.

use std::process::Command;

fn sqlx_features(extra: &[&str]) -> Vec<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["tree", "--locked", "--offline", "-p", "persistant-sqlx"])
        .args(extra)
        .args(["-e", "normal,build", "-i", "sqlx", "--format", "{p} [{f}]"])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).expect("utf-8");
    // First line: `sqlx v0.9.0 [feature,feature,...]`.
    let first = text.lines().next().expect("cargo tree printed nothing");
    assert!(first.starts_with("sqlx "), "unexpected first line: {first}");
    let open = first.find('[').expect("no feature list");
    first[open + 1..first.len() - 1]
        .split(',')
        .map(str::to_owned)
        .collect()
}

fn check(features: &[String]) {
    // Positive control: the list is read (sqlx's own `postgres` feature is in it).
    assert!(
        features.iter().any(|f| f == "postgres"),
        "feature list not read: {features:?}"
    );
    assert!(
        !features.iter().any(|f| f == "time"),
        "persistant-sqlx enables sqlx's `time` feature, which breaks sqlx::query! for a chrono consumer: {features:?}"
    );
}

#[test]
fn default_build_leaves_sqlx_time_off() {
    check(&sqlx_features(&[]));
}

#[test]
fn all_features_leave_sqlx_time_off() {
    check(&sqlx_features(&["--all-features"]));
}
