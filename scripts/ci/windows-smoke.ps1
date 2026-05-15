param(
  [switch]$SkipInstall
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
Set-Location $RepoRoot

function Invoke-Step {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Command,
    [string[]]$Arguments = @()
  )

  Write-Host ""
  Write-Host "==> $Command $($Arguments -join ' ')"
  & $Command @Arguments
  if ($LASTEXITCODE -ne 0) {
    throw "$Command exited $LASTEXITCODE"
  }
}

if (-not $SkipInstall) {
  Invoke-Step pnpm @("install", "--frozen-lockfile")
}

$EvidenceDir = Join-Path ([System.IO.Path]::GetTempPath()) "pice-windows-smoke-$PID"
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$env:PICE_RELEASE_SMOKE_EVIDENCE = Join-Path $EvidenceDir "release-artifact-smoke-evidence.json"
Write-Host "Windows smoke evidence path: $EvidenceDir"

Invoke-Step pnpm @("build")

$DebugBin = Join-Path $RepoRoot "target\debug"
$env:PATH = "$DebugBin;$env:PATH"
$env:RUST_TEST_THREADS = "1"

Invoke-Step cargo @("clippy", "--", "-D", "warnings")
Invoke-Step cargo @("test", "--", "--skip", "parallel_cohort_meets_16x_speedup")
Invoke-Step cargo @("build", "--release", "-p", "pice-cli", "-p", "pice-daemon")

Invoke-Step pnpm @("exec", "vitest", "run", "scripts/acceptance/release-artifact-smoke.test.mjs")

$env:PICE_NPM_PACK_SMOKE = "1"
Invoke-Step node @("scripts/acceptance/release-artifact-smoke.mjs")
