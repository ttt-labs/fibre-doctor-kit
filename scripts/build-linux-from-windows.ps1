param([switch]$Tests)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$rustBin = Join-Path $env:USERPROFILE '.cargo\bin'
$portableCompiler = Join-Path $projectRoot '.tools\w64devkit\bin'
if (Test-Path -LiteralPath $portableCompiler) { $env:PATH = $portableCompiler + ';' + $env:PATH }
$env:PATH = $rustBin + ';' + $env:PATH
Push-Location $projectRoot
try {
    $sysroot = (& rustc --print sysroot).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain unavailable' }
    $hostTriple = ((& rustc -vV | Select-String '^host:').Line -replace '^host:\s*','').Trim()
    if ($hostTriple -eq 'x86_64-pc-windows-gnu') {
        $wrapperDir = Join-Path $projectRoot 'target\build-tools'
        New-Item -ItemType Directory -Force -Path $wrapperDir | Out-Null
        $wrapper = Join-Path $wrapperDir 'rustc-self-contained.exe'
        & rustc -C link-self-contained=yes (Join-Path $PSScriptRoot 'windows-rustc-wrapper.rs') -o $wrapper
        if ($LASTEXITCODE -ne 0) { throw 'Host compiler wrapper build failed' }
        $env:RUSTC_WRAPPER = $wrapper
    }
    $linker = Join-Path $sysroot ('lib\rustlib\' + $hostTriple + '\bin\rust-lld.exe')
    if (!(Test-Path -LiteralPath $linker)) { throw 'Bundled LLVM linker unavailable' }
    $env:CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = $linker
    $env:CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = '-C linker-flavor=ld.lld -C link-self-contained=yes -C target-feature=+crt-static'
    & cargo clippy --locked --all-targets --target x86_64-unknown-linux-musl -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Linux clippy failed' }
    & cargo build --locked --release --target x86_64-unknown-linux-musl
    if ($LASTEXITCODE -ne 0) { throw 'Linux release build failed' }
    if ($Tests) {
        & cargo test --locked --no-run --target x86_64-unknown-linux-musl
        if ($LASTEXITCODE -ne 0) { throw 'Linux test compilation failed' }
    }
} finally { Pop-Location }
