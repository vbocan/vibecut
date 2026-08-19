# Dry-run validate a TOML config — parses + cross-field validates, no
# rendering. Useful for syntax checks and "did I spell that field
# right?" sanity testing.
#
# Usage:
#   scripts\validate.ps1 examples\masinuta.toml
#   scripts\validate.ps1 examples\masinuta.toml -PrintEffectiveConfig

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Config,
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
    cargo build --release -p vibecut --quiet
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$cliArgs = @('run', $Config, '--dry-run')
if ($PrintEffectiveConfig) { $cliArgs += '--print-effective-config' }

& $bin @cliArgs
exit $LASTEXITCODE
