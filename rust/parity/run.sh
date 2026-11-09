#!/usr/bin/env bash
# Parity run: builds the Rust runner, runs both implementations on rust/parity/corpus/*.jsonl, diffs.
# Usage: rust/parity/run.sh [--regen]   (--regen rewrites the corpus first). Exit 1 when any case differs.
# Honors CARGO_TARGET_DIR. Outputs go to rust/parity/out/ (ignored by git).
set -euo pipefail
cd "$(dirname "$0")/../.."
[ "${1:-}" = "--regen" ] && .venv/bin/python -I rust/parity/gen_corpus.py
cargo build --quiet --manifest-path rust/Cargo.toml --bin parity
bin="$(cargo metadata --manifest-path rust/Cargo.toml --format-version 1 --no-deps | .venv/bin/python -I -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')/debug/parity"
mkdir -p rust/parity/out
.venv/bin/python -I -W ignore rust/parity/run_py.py rust/parity/corpus/*.jsonl > rust/parity/out/py.jsonl
"$bin" rust/parity/corpus/*.jsonl > rust/parity/out/rust.jsonl
.venv/bin/python -I rust/parity/diff.py rust/parity/out/py.jsonl rust/parity/out/rust.jsonl rust/parity/corpus
