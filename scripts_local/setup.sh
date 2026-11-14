#!/usr/bin/env bash
# One-time setup: Python env for dbt + MetricFlow, and the Rust CLI.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
command -v uv >/dev/null || { echo "uv is required: https://docs.astral.sh/uv/"; exit 1; }
(cd "$ROOT/analytics" && uv sync --python 3.14)
(cd "$ROOT/app" && cargo build --release)
echo "Setup done. Next: ./scripts_local/run_pipeline.sh"
