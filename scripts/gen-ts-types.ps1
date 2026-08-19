# Regenerate the TypeScript types mirroring Config/Manifest
# (vibecut) and FeedbackAction (vibecut-ops). Runs the ts-rs
# export hook via `cargo test`; each crate's own bindings/*.ts is
# gitignored (derived output, not source), then copied into
# src/vibecut-ui/src/lib/types/generated/ so the SvelteKit app has a
# single place to import them from.
#
# The `ts-export` feature is off by default on both crates so the
# production/Docker build never pulls in ts-rs; this script is the
# only place that turns it on.
#
# Usage:
#   scripts\gen-ts-types.ps1

[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

cargo test -p vibecut --features ts-export --lib -- export_bindings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo test -p vibecut-ops --features ts-export --lib -- export_bindings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$dest = Join-Path $repoRoot 'src\vibecut-ui\src\lib\types\generated'
if (Test-Path (Join-Path $repoRoot 'src\vibecut-ui')) {
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Get-ChildItem (Join-Path $repoRoot 'src\vibecut\bindings') -Filter '*.ts' |
        Copy-Item -Destination $dest -Force
    Get-ChildItem (Join-Path $repoRoot 'src\vibecut-ops\bindings') -Filter '*.ts' |
        Copy-Item -Destination $dest -Force
    $n = (Get-ChildItem $dest -Filter '*.ts' | Measure-Object).Count
    Write-Host "`n$n type(s) copied to $dest" -ForegroundColor Green
} else {
    Write-Host "`nsrc/vibecut-ui/ not found yet — types written to src/vibecut/bindings and src/vibecut-ops/bindings only." -ForegroundColor Yellow
}
