<#
.SYNOPSIS
Build the nestris wasm package for the web app.

.DESCRIPTION
Default build enables WebAssembly SIMD (simd128), supported by all evergreen
browsers since 2023; wasm-opt -O4 runs via the wasm-pack profile. The output
lands in crates/nestris-wasm/pkg, which web/ consumes as a file: dependency.

RUSTFLAGS are set only for this invocation (not via .cargo/config.toml), so
native builds and flag precedence stay unambiguous.

.PARAMETER Compat
Also produce a non-SIMD fallback package in crates/nestris-wasm/pkg-compat
for very old browsers. Not built by default and not wired into the app;
see docs/USAGE.md if you need to serve it.

.EXAMPLE
./tools/build-wasm.ps1
./tools/build-wasm.ps1 -Compat
#>
param(
    [switch]$Compat
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    Write-Host "Building nestris-wasm (simd128) -> crates/nestris-wasm/pkg" -ForegroundColor Cyan
    $env:RUSTFLAGS = "-C target-feature=+simd128"
    wasm-pack build crates/nestris-wasm --target web --release
    if ($LASTEXITCODE -ne 0) { throw "wasm-pack (simd) failed" }

    if ($Compat) {
        Write-Host "Building compat (no SIMD) -> crates/nestris-wasm/pkg-compat" -ForegroundColor Cyan
        $env:RUSTFLAGS = ""
        wasm-pack build crates/nestris-wasm --target web --release --out-dir pkg-compat
        if ($LASTEXITCODE -ne 0) { throw "wasm-pack (compat) failed" }
    }
}
finally {
    Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    Pop-Location
}
Write-Host "Done." -ForegroundColor Green
