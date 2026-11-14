# seed -> dbt build -> LLM extraction -> demo.
#   -Mock    use the offline extractor instead of LM Studio
#   -Seed N  choose the data seed (default 42)
param([switch]$Mock, [int]$Seed = 42)
$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
$S2r = "$Root/app/target/release/s2r.exe"
if (-not (Test-Path $S2r)) { Push-Location "$Root/app"; cargo build --release; Pop-Location }
$Extractor = if ($Mock) { "mock" } else { "lmstudio" }
Push-Location $Root
try {
  & $S2r seed --seed $Seed; if ($LASTEXITCODE) { throw "seed failed" }
  Push-Location analytics
  try { $env:DBT_PROFILES_DIR = "."; & .venv/Scripts/dbt.exe build --quiet; if ($LASTEXITCODE) { throw "dbt build failed" } } finally { Pop-Location }
  & $S2r extract --extractor $Extractor; if ($LASTEXITCODE) { throw "extract failed" }
  & $S2r demo; if ($LASTEXITCODE) { throw "demo failed" }
} finally { Pop-Location }
