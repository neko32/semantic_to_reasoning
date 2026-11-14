# Everything CI runs, plus the coverage report.
#   -Live   also run the tests that call a real LM Studio server
param([switch]$Live)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
function Step($cmd) { Write-Host "> $cmd"; Invoke-Expression $cmd; if ($LASTEXITCODE) { throw "failed: $cmd" } }
Push-Location "$Root/app"
try {
  Step "cargo fmt --check"
  Step "cargo clippy --all-targets -- -D warnings"
  Step "cargo llvm-cov --fail-under-lines 90 --html --output-dir `"$Root/docs/coverage`""
  Step "cargo llvm-cov report --summary-only"
  Step "cargo audit"
  Step "cargo test --test integration -- --ignored"
  if ($Live) { Step "cargo test --lib -- --ignored live_" }
} finally { Pop-Location }
Write-Host "All checks passed."
