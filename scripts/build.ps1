# Build the vibecut CLI binary on the host (release profile).
#
# This is the binary `scripts/run-cli.ps1` and friends shell out to.
# Builds the full workspace; the vibecut binary lands at
# `target/release/vibecut.exe` on Windows.

param(
    [switch]$Clean
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

if ($Clean) {
    Write-Host "cleaning target/..." -ForegroundColor Cyan
    cargo clean
}

Write-Host "cargo build --release" -ForegroundColor Cyan
cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$bin = Resolve-Path "target/release/vibecut.exe"
Write-Host "`nbuilt: $bin" -ForegroundColor Green
& $bin --version
