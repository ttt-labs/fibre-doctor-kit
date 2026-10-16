param([switch]$Release)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$rustBin = Join-Path $env:USERPROFILE '.cargo\bin'
$portableCompiler = Join-Path $projectRoot '.tools\w64devkit\bin'
if (Test-Path -LiteralPath $portableCompiler) { $env:PATH = $portableCompiler + ';' + $env:PATH }
if (Test-Path -LiteralPath $rustBin) { $env:PATH = $rustBin + ';' + $env:PATH }
Push-Location $projectRoot
try {
    $hostTriple = ((& rustc -vV | Select-String '^host:').Line -replace '^host:\s*','').Trim()
    if ($hostTriple -eq 'x86_64-pc-windows-gnu') {
        $env:RUSTFLAGS = '-C link-self-contained=yes'
    }
    & cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    & cargo clippy --locked --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Clippy failed' }
    if ($Release) { & cargo build --locked --release } else { & cargo build --locked }
    if ($LASTEXITCODE -ne 0) { throw 'Build failed' }
} finally { Pop-Location }
