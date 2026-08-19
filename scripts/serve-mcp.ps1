# Start the MCP server in detached mode. Bind-mounts host paths into
# the container; defaults assume the masinuta footage and the in-repo
# `out/mcp/` projects directory, but both are overridable.
#
# Usage:
#   scripts\serve-mcp.ps1
#   scripts\serve-mcp.ps1 -Footage "E:\different\footage" -Projects "C:\elsewhere\projects"
#   scripts\serve-mcp.ps1 -Stop
#
# After the server is up, point Claude Desktop / LM Studio at
# http://localhost:8080/.

[CmdletBinding()]
param(
    [string]$Footage  = 'E:/DATA/GOPRO VIDEO/2020.04 - Ne dăm cu mașinuța electrică',
    [string]$Projects = (Join-Path (Split-Path -Parent $PSScriptRoot) 'out\mcp'),
    [int]   $Port     = 8080,
    [switch]$NoGpu,
    [switch]$Stop,
    [string]$Name     = 'vibecut'
)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

if ($Stop) {
    Write-Host "stopping container $Name..." -ForegroundColor Cyan
    docker stop $Name 2>$null
    Write-Host "stopped." -ForegroundColor Green
    exit 0
}

if (-not (Test-Path $Footage)) {
    Write-Host "ERROR: footage path not found: $Footage" -ForegroundColor Red
    exit 2
}
New-Item -ItemType Directory -Path $Projects -Force | Out-Null

# Best-effort cleanup of any leftover container with the same name.
docker rm -f $Name 2>$null | Out-Null

$dockerArgs = @('run', '-d', '--rm', '--name', $Name,
                '-p', "127.0.0.1:${Port}:8080",
                '-v', "${Footage}:/footage:ro",
                '-v', "${Projects}:/projects",
                '-v', 'vibecut-cache:/cache')
if (-not $NoGpu) { $dockerArgs += @('--gpus', 'all') }
$dockerArgs += 'vibecut:latest'

Write-Host "docker $($dockerArgs -join ' ')`n" -ForegroundColor Cyan
$id = docker @dockerArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "container: $($id.Substring(0, [Math]::Min(12, $id.Length)))" -ForegroundColor Green

# Wait for the HTTP listener.
$deadline = (Get-Date).AddSeconds(30)
while ((Get-Date) -lt $deadline) {
    try {
        $null = Invoke-WebRequest -Uri "http://127.0.0.1:$Port/" -UseBasicParsing `
            -TimeoutSec 2 -ErrorAction Stop
        break
    } catch {
        # 404 / 405 means the server is up but doesn't handle GET-without-init —
        # which is the normal MCP behaviour.
        if ($_.Exception.Response -ne $null) { break }
        Start-Sleep -Milliseconds 400
    }
}

Write-Host "`nMCP server listening on http://localhost:$Port/" -ForegroundColor Green
Write-Host "wire it up:  $env:APPDATA\Claude\claude_desktop_config.json" -ForegroundColor Yellow
Write-Host '    {"mcpServers":{"vibecut":{"url":"http://localhost:'"$Port"'/"}}}' -ForegroundColor Yellow
Write-Host "`nstop with:   scripts\serve-mcp.ps1 -Stop" -ForegroundColor Yellow
