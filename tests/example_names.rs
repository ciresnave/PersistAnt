// SPDX-License-Identifier: MIT OR Apache-2.0
//! No two workspace members declare an example of the same name.
//!
//! Cargo writes every example to `target/<profile>/examples/<name>.exe`, whatever package it is in.
//! Two members with an example `matrix` collide on that file; cargo only warns ("output filename
//! collision"), and on Windows a parallel build then fails at random with
//! `LNK1104: cannot open file '...\examples\matrix.exe'` (PersistAnt#181, run 41104490672). So the
//! names must be unique, and this test fails when they are not.

use std::collections::BTreeMap;
use std::path::Path;

/// Every example a member declares: `examples/<name>.rs`, `examples/<name>/main.rs`, and each
/// `[[example]] name = "..."` in its `Cargo.toml`. Returns `(package directory, example name)`.
fn examples_of(dir: &Path, label: &str, out: &mut Vec<(String, String)>) {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir.join("examples")) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() && p.extension().is_some_and(|x| x == "rs") {
                names.push(p.file_stem().unwrap().to_string_lossy().into_owned());
            } else if p.is_dir() && p.join("main.rs").is_file() {
                names.push(p.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    if let Ok(toml) = std::fs::read_to_string(dir.join("Cargo.toml")) {
        let mut in_example = false;
        for line in toml.lines() {
            let t = line.trim();
            if t.starts_with('[') {
                in_example = t == "[[example]]";
            } else if in_example && let Some(rest) = t.strip_prefix("name") {
                let v = rest
                    .trim_start()
                    .trim_start_matches('=')
                    .trim()
                    .trim_matches('"');
                names.push(v.to_string());
            }
        }
    }
    names.sort();
    names.dedup(); // a declared example that is also auto-discovered is one example
    out.extend(names.into_iter().map(|n| (label.to_string(), n)));
}

/// The example names declared by more than one package, with the packages.
fn duplicates(all: &[(String, String)]) -> BTreeMap<String, Vec<String>> {
    let mut by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pkg, name) in all {
        by_name.entry(name.clone()).or_default().push(pkg.clone());
    }
    by_name.retain(|_, pkgs| pkgs.len() > 1);
    by_name
}

#[test]
fn the_scan_finds_the_collision_the_old_names_had() {
    // Positive control: the two pairs that broke Windows CI, fed to the same function.
    let old = [
        ("persistant-opendal", "matrix"),
        ("persistant-opendal", "speed"),
        ("persistant-opendal", "speed_concurrent"),
        ("persistant-sqlx", "matrix"),
        ("persistant-sqlx", "speed"),
        ("persistant-sqlx", "matrix_sqlite"),
    ]
    .map(|(p, n)| (p.to_string(), n.to_string()));
    let d = duplicates(&old);
    assert_eq!(
        d.keys().map(String::as_str).collect::<Vec<_>>(),
        ["matrix", "speed"]
    );
    assert_eq!(d["matrix"], ["persistant-opendal", "persistant-sqlx"]);
}

#[test]
fn no_two_members_declare_an_example_of_the_same_name() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut all = Vec::new();
    examples_of(root, "persistant", &mut all);
    let crates = std::fs::read_dir(root.join("crates")).expect("crates/ exists");
    let mut dirs: Vec<_> = crates
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in &dirs {
        let label = d.file_name().unwrap().to_string_lossy().into_owned();
        examples_of(d, &label, &mut all);
    }
    // The scan saw examples at all (a scan of nothing would pass for the wrong reason).
    assert!(
        all.iter().any(|(p, n)| p == "persistant" && n == "hello"),
        "the scan did not find the root example `hello`: {all:?}"
    );
    assert!(all.len() > 10, "the scan found only {} examples", all.len());
    let dup = duplicates(&all);
    assert!(
        dup.is_empty(),
        "example names declared by more than one package (they share target/<profile>/examples/<name>.exe): {dup:?}"
    );
}
