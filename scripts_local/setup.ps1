# One-time setup: Python env for dbt + MetricFlow, and the Rust CLI.
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
if (-not (Get-Command uv -ErrorAction SilentlyContinue)) { throw "uv is required: https://docs.astral.sh/uv/" }
Push-Location "$Root/analytics"; try { uv sync --python 3.14; if ($LASTEXITCODE) { throw "uv sync failed" } } finally { Pop-Location }
Push-Location "$Root/app"; try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location }
Write-Host "Setup done. Next: .\scripts_local\run_pipeline.ps1"
