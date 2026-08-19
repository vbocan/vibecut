# Probe a video file or folder with `vibecut probe`.
#
# Usage:
#   scripts\probe.ps1 E:\footage
#   scripts\probe.ps1 E:\footage -Json
#   scripts\probe.ps1 E:\footage\clip.mp4
#
# When -Json is passed, the output is machine-readable JSON suitable
# for piping into jq, ConvertFrom-Json, or another tool.

[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Path,
    [switch]$Json,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

if (-not (Test-Path $Path)) {
    Write-Host "ERROR: path not found: $Path" -ForegroundColor Red
    exit 2
}

$bin = Join-Path $repoRoot 'target\release\vibecut.exe'
if (-not $SkipBuild) {
    cargo build --release -p vibecut --quiet
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$cliArgs = @('probe', $Path)
if ($Json) { $cliArgs += '--json' }

& $bin @cliArgs
exit $LASTEXITCODE
