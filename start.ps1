# Build vibecut (release) and render the Egypt example.
# Run from any directory; cwd is anchored to this script's folder.

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

cargo build --release
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$env:VIBECUT_VISUAL_WORKERS = '6'
cargo run --release -p vibecut -- run examples/egypt.toml
