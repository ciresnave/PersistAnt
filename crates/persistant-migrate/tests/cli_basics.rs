// SPDX-License-Identifier: MIT OR Apache-2.0
//! The command-line basics every tool answers (CodeRipper OPS-011 `cli-basics`): `--version` and
//! `--help` print and exit 0 before any other parsing, and an unknown flag or subcommand exits 2
//! without touching a database. Run through the real binary.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(args)
        .env_remove("PERSISTANT_DATABASE_URL")
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn version_prints_the_crate_version_and_exits_zero() {
    for flag in ["--version", "-V"] {
        let o = run(&[flag]);
        assert_eq!(o.status.code(), Some(0), "{flag}: {o:?}");
        let text = out(&o);
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{flag}: {text}");
        assert!(text.starts_with("persistant-migrate "), "{flag}: {text}");
    }
}

#[test]
fn help_prints_the_usage_on_stdout_and_exits_zero_even_after_a_subcommand() {
    for args in [
        &["--help"][..],
        &["-h"][..],
        &["up", "--help"][..],
        &["snapshot", "--dir", "x", "--help"][..],
    ] {
        let o = run(args);
        assert_eq!(o.status.code(), Some(0), "{args:?}: {o:?}");
        assert!(out(&o).contains("usage: persistant-migrate"), "{args:?}");
    }
}

#[test]
fn a_bogus_flag_or_subcommand_exits_two_and_names_the_usage() {
    for args in [
        &["--bogus"][..],
        &["bogus"][..],
        &["up", "--bogus"][..],
        &[][..],
    ] {
        let o = run(args);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {o:?}");
        assert!(out(&o).is_empty(), "{args:?}: usage errors go to stderr");
        assert!(
            String::from_utf8_lossy(&o.stderr).contains("usage: persistant-migrate"),
            "{args:?}"
        );
    }
}

#[test]
fn an_unknown_subcommand_is_refused_before_any_connection_even_with_a_url_set() {
    // Known-bad control for the ordering: a URL that cannot connect would exit 7, not 2, if the
    // subcommand were checked only after connecting.
    let o = Command::new(env!("CARGO_BIN_EXE_persistant-migrate"))
        .args(["bogus", "--dir", "x"])
        .env(
            "PERSISTANT_DATABASE_URL",
            "postgres://nobody@127.0.0.1:1/none",
        )
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2), "{o:?}");
}
