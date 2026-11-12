#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Check that the checked-in matrices carry this crate's MAJOR.MINOR.

    python tools/check-matrix-version.py [--root DIR]
    python tools/check-matrix-version.py --self-test

The `kv-on-blob` and `kv-on-relational` rows of `matrix/*.tsv` record the version of the crate that
certified them as `<MAJOR>.<MINOR>+<driver version>` (`crates/persistant-kv/src/on_blob.rs`,
`on_relational.rs`, built from `CARGO_PKG_VERSION_MAJOR` and `_MINOR`). A minor bump (0.16 -> 0.17)
leaves every such row stale: the registry then finds no row ("NoRow") and every provider is unmet. A
patch bump changes nothing here. On a mismatch this exits 1 with

    regenerate matrix/*.tsv for <MAJOR.MINOR>

(the generator's output is the CI artifact `matrix-<name>` of a run on the stamped head; copy it
unedited). Standard library only; the files are enumerated with `git ls-files`.
"""
import os
import re
import subprocess
import sys


def workspace_major_minor(cargo_toml_text):
    """MAJOR.MINOR of `[workspace.package] version`, or of `[package] version`."""
    for table in ("workspace.package", "package"):
        m = re.search(r"(?ms)^\[" + re.escape(table) + r"\]\s*$(.*?)(?=^\[|\Z)", cargo_toml_text)
        if m:
            v = re.search(r'(?m)^\s*version\s*=\s*"(\d+)\.(\d+)\.\d+', m.group(1))
            if v:
                return f"{v.group(1)}.{v.group(2)}"
    return None


def stale_rows(tsv_text, want):
    """(line number, version) of every `kv-on-*` row whose recorded MAJOR.MINOR is not `want`."""
    bad = []
    for n, line in enumerate(tsv_text.splitlines(), 1):
        cols = line.split("\t")
        if len(cols) < 4 or not cols[2].startswith("kv-on-"):
            continue
        m = re.match(r"(\d+\.\d+)\+", cols[3])
        if not m or m.group(1) != want:
            bad.append((n, cols[3]))
    return bad


def tracked_matrices(root):
    out = subprocess.run(
        ["git", "-C", root, "ls-files", "matrix/*.tsv"], capture_output=True, text=True, check=True
    )
    return [p for p in out.stdout.splitlines() if p]


def check(root):
    with open(os.path.join(root, "Cargo.toml"), encoding="utf-8") as f:
        want = workspace_major_minor(f.read())
    if not want:
        print("check-matrix-version: no version in Cargo.toml", file=sys.stderr)
        return 2
    files = tracked_matrices(root)
    if not files:
        print("check-matrix-version: no matrix/*.tsv is tracked", file=sys.stderr)
        return 2
    rows = 0
    stale = False
    for rel in files:
        with open(os.path.join(root, rel), encoding="utf-8") as f:
            text = f.read()
        rows += sum(1 for l in text.splitlines() if len(l.split("\t")) > 2 and l.split("\t")[2].startswith("kv-on-"))
        for n, v in stale_rows(text, want)[:3]:
            print(f"{rel}:{n}: version {v}, the crate is {want}", file=sys.stderr)
            stale = True
    if rows == 0:
        # A check that sees no kv-on-* row asserts nothing.
        print("check-matrix-version: no kv-on-* row found in matrix/*.tsv", file=sys.stderr)
        return 2
    if stale:
        print(f"regenerate matrix/*.tsv for {want}", file=sys.stderr)
        return 1
    print(f"check-matrix-version: ok ({rows} kv-on-* rows at {want})")
    return 0


def self_test():
    """The check must be able to fail: a stale row is caught, a current row and a non-kv row are not."""
    head = "subject_kind\tsubject\tbackend\tversion\n"
    cur = "capability\tkv.get\tkv-on-blob:opendal-fs\t0.17+0.59.5\n"
    old = "capability\tkv.get\tkv-on-blob:opendal-fs\t0.16+0.59.5\n"
    plain = "capability\tblob.read\topendal-fs\t0.59.5\n"
    assert stale_rows(head + cur + plain, "0.17") == []
    assert stale_rows(head + old, "0.17") == [(2, "0.16+0.59.5")]
    assert workspace_major_minor('[workspace.package]\nversion = "0.17.3"\n') == "0.17"
    print("check-matrix-version: self-test ok")
    return 0


def main():
    args = sys.argv[1:]
    if args == ["--self-test"]:
        return self_test()
    root = "."
    if args[:1] == ["--root"] and len(args) == 2:
        root = args[1]
    elif args:
        print(__doc__)
        return 2
    return check(root)


if __name__ == "__main__":
    sys.exit(main())
