# Windows cross-build for 32-bit ARM Linux (armv7).
#
# Needs zig and cargo-zigbuild. See doc/build-linux-arm-on-windows.md.

$ErrorActionPreference = "Stop"

$targets = @(
    "armv7-unknown-linux-musleabihf",
    "armv7-unknown-linux-gnueabihf"
)

# Full LTO is fine natively but can stall an ARM cross-link for a very long
# time; release size optimisation stays on.
$env:CARGO_PROFILE_RELEASE_LTO = "off"
$env:CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16"

$projectRoot = Split-Path -Parent $PSScriptRoot
$localZig = Join-Path $projectRoot ".tools\zig"
if (Test-Path (Join-Path $localZig "zig.exe")) {
    $env:PATH = "$localZig;$(Join-Path $projectRoot '.tools\cargo-zigbuild\bin');$env:PATH"
}

if (-not (Get-Command cargo-zigbuild -ErrorAction SilentlyContinue)) {
    throw "cargo-zigbuild is required: cargo install cargo-zigbuild"
}
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    throw "zig is required and must be on PATH"
}

foreach ($target in $targets) {
    rustup target add $target
}

Push-Location $projectRoot
try {
    foreach ($target in $targets) {
        Write-Host "==> $target"
        cargo zigbuild --release --target $target --bin tiny-frpc --bin tiny-frpc-ssh
        if ($LASTEXITCODE -ne 0) { throw "build failed for $target" }
    }

    Write-Host ""
    Write-Host "artifacts:"
    foreach ($target in $targets) {
        Get-ChildItem "target\$target\release\tiny-frpc", "target\$target\release\tiny-frpc-ssh" |
            Select-Object FullName, @{n = "MB"; e = { [math]::Round($_.Length / 1MB, 2) } }
    }
}
finally {
    Pop-Location
}
