# Build the all-in-one Docker image (CUDA runtime + ffmpeg with NVENC
# + vibecut binary + Rust MCP server). Uses the multi-stage
# Dockerfile at the repo root.
#
# First build pulls + compiles everything (~5–10 min); subsequent
# builds are layer-diffed (~30 s for a Rust source change).

[CmdletBinding()]
param(
    [string]$Tag = 'vibecut:latest',
    [switch]$NoCache
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$args = @('build', '-t', $Tag, '-f', 'Dockerfile')
if ($NoCache) { $args += '--no-cache' }
$args += '.'

Write-Host "docker $($args -join ' ')" -ForegroundColor Cyan
docker @args
exit $LASTEXITCODE
