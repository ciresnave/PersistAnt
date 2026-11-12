kind: fix: a test no longer trips clippy err_expect on Rust 1.100
---
`tests/blocking_registry.rs` used `r.err().expect(..)`, which `clippy::err_expect` (new in Rust 1.100) rejects under `--all-features`; it now matches `let Err(e) = r else { panic!(..) }` (the Ok type is not `Debug`, so `expect_err` does not apply). Test-only. Verified: `cargo +stable clippy --workspace --all-targets --all-features -- -D warnings` fails before on this line and is clean after.
