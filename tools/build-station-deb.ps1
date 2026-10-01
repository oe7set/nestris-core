<#
.SYNOPSIS
Build the nestris-station Debian package (.deb) on Windows via Docker.

.DESCRIPTION
Builds inside a Debian bookworm Rust container, so the binary links against
bookworm's glibc and runs on Debian 12 and 13 (amd64). The source tree is
mounted read-only; the cargo target dir, cargo home (registry, cargo-deb)
and rustup toolchains live in Docker volumes (nestris-station-*) so rebuilds
are incremental. `docker volume rm nestris-station-target
nestris-station-cargo nestris-station-rustup` resets them. The finished package lands in dist/.

Needs Docker Desktop running (Linux containers). Without Docker, run the same
commands in a WSL Debian/Ubuntu shell:
    cargo install cargo-deb --locked
    cargo deb -p nestris-station          # add --features tls for MQTT over TLS

.PARAMETER Tls
Build with MQTT over TLS support (feature `tls`).

.EXAMPLE
./tools/build-station-deb.ps1
./tools/build-station-deb.ps1 -Tls
#>
param(
    [switch]$Tls
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root "dist"
New-Item -ItemType Directory -Force $dist | Out-Null

docker version --format '{{.Server.Version}}' *> $null
if ($LASTEXITCODE -ne 0) {
    throw "Docker is not running. Start Docker Desktop (Linux containers) and retry."
}

$features = if ($Tls) { "--features tls" } else { "" }
$script = @"
set -e
command -v cargo-deb >/dev/null || cargo install cargo-deb --locked
cargo deb -p nestris-station $features --output /out/
"@ -replace "`r", ""

Write-Host "Building nestris-station .deb (Debian bookworm, amd64) -> dist/" -ForegroundColor Cyan
docker run --rm `
    -v "${root}:/src:ro" `
    -v "${dist}:/out" `
    -v nestris-station-target:/target `
    -v nestris-station-cargo:/usr/local/cargo `
    -v nestris-station-rustup:/usr/local/rustup `
    -e CARGO_TARGET_DIR=/target `
    -w /src `
    rust:1-bookworm `
    sh -c $script
if ($LASTEXITCODE -ne 0) { throw "docker build failed ($LASTEXITCODE)" }

Get-ChildItem $dist -Filter "nestris-station_*.deb" |
    Sort-Object LastWriteTime -Descending |
    Select-Object -First 1 |
    ForEach-Object { Write-Host "OK: $($_.FullName)" -ForegroundColor Green }
