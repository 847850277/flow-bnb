#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
for file in flows/*.http.yml; do
  cargo run --locked --quiet -- check "$file"
done
cargo build --locked --bins
for scenario in success decline wallet-reject timeout expired wrong-chain cancel; do
  python3 scripts/demo_handoff.py --self-test --no-build --scenario "$scenario"
done
