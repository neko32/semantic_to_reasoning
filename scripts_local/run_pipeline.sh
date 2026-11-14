#!/usr/bin/env bash
# seed -> dbt build -> LLM extraction -> demo.
#   --mock   use the offline extractor instead of LM Studio
#   --seed N choose the data seed (default 42)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
EXTRACTOR=lmstudio; SEED=42
while [[ $# -gt 0 ]]; do
  case "$1" in
    --mock) EXTRACTOR=mock ;;
    --seed) SEED="$2"; shift ;;
    *) echo "unknown option $1"; exit 2 ;;
  esac
  shift
done
S2R="$ROOT/app/target/release/s2r"
[[ -x "$S2R" ]] || (cd "$ROOT/app" && cargo build --release)
cd "$ROOT"
"$S2R" seed --seed "$SEED"
(cd analytics && DBT_PROFILES_DIR=. .venv/bin/dbt build --quiet)
"$S2R" extract --extractor "$EXTRACTOR"
"$S2R" demo
