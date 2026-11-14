#!/usr/bin/env bash
# Everything CI runs, plus the coverage report.
#   --live   also run the tests that call a real LM Studio server
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/app"
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo llvm-cov --fail-under-lines 90 --html --output-dir "$ROOT/docs/coverage"
cargo llvm-cov report --summary-only
cargo audit
cargo test --test integration -- --ignored
[[ "${1:-}" == "--live" ]] && cargo test --lib -- --ignored live_
echo "All checks passed."
