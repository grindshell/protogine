# Build a self-contained release example under the ignored target directory.
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$distribution = Join-Path $projectRoot 'target/native-distance-demo'
$gameRoot = Join-Path $distribution 'game'
New-Item -ItemType Directory -Force -Path (Join-Path $gameRoot 'plugins') | Out-Null
$compiler = if ($env:CLANG) { $env:CLANG } else { 'clang' }
& $compiler --target=x86_64-pc-windows-msvc -std=c11 -shared -O2 -Werror `
    -I (Join-Path $projectRoot 'include') `
    (Join-Path $projectRoot 'examples/plugins/grid_distance.c') `
    -o (Join-Path $gameRoot 'plugins/grid_distance.dll')
if ($LASTEXITCODE -ne 0) { throw 'C example compilation failed' }
foreach ($file in @('main.luau', 'distance.luau', 'game.tot', 'SCHEMA.md')) {
    Copy-Item -LiteralPath (Join-Path $projectRoot "examples/games/native_distance/$file") -Destination $gameRoot -Force
}
Push-Location $projectRoot
try {
    cargo build --offline --release --bin protogine-player --example native_benchmark
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
} finally {
    Pop-Location
}
Copy-Item -LiteralPath (Join-Path $projectRoot 'target/release/protogine-player.exe') -Destination $distribution -Force
Write-Output "Built $distribution"
