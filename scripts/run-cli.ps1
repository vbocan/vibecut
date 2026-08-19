# Render a TOML config with the vibecut CLI — no MCP, no LLM.
#
# Usage:
#   scripts\run-cli.ps1 my-project.toml
#   scripts\run-cli.ps1 my-project.toml -RandomSeed
#   scripts\run-cli.ps1 my-project.toml -Explain
#
# Auto-builds the binary if it's missing or the source has changed
# since the last build. Pass -SkipBuild to use whatever's already at
# target/release/vibecut.exe.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Config,
    [switch]$DryRun,
    [switch]$Explain,
    [switch]$NoVisual,
    [switch]$NoCache,
    [switch]$RandomSeed,
    [switch]$PrintEffectiveConfig,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not (Test-Path $Config)) {
    Write-Host "ERROR: config not found: $Config" -ForegroundColor Red
    exit 2
}

$bin = Join-Path $repoRoot 'target\release\vibecut.exe'
if (-not $SkipBuild) {
    Write-Host "cargo build --release -p vibecut" -ForegroundColor Cyan
    cargo build --release -p vibecut
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not (Test-Path $bin)) {
    Write-Host "ERROR: binary missing at $bin (build may have failed)" -ForegroundColor Red
    exit 1
}

# Translate switch params to CLI flags. Order matches the vibecut
# binary's clap layout.
$cliArgs = @('run', $Config)
if ($DryRun)               { $cliArgs += '--dry-run' }
if ($Explain)              { $cliArgs += '--explain' }
if ($NoVisual)             { $cliArgs += '--no-visual' }
if ($NoCache)              { $cliArgs += '--no-cache' }
if ($RandomSeed)           { $cliArgs += '--random-seed' }
if ($PrintEffectiveConfig) { $cliArgs += '--print-effective-config' }

Write-Host "`n$bin $($cliArgs -join ' ')`n" -ForegroundColor Cyan
& $bin @cliArgs
exit $LASTEXITCODE
