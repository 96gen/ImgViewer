#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$VcpkgRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "The codec helper process test requires Windows."
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne
    [System.Runtime.InteropServices.Architecture]::X64) {
    throw "The codec helper process test requires Windows x64."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $repoRoot "src-tauri\Cargo.toml"
$vcpkgManifestPath = Join-Path $repoRoot "vcpkg.json"
if ([string]::IsNullOrWhiteSpace($VcpkgRoot)) {
    if (-not [string]::IsNullOrWhiteSpace($env:VCPKG_ROOT)) {
        $VcpkgRoot = $env:VCPKG_ROOT
    } else {
        $VcpkgRoot = Join-Path $repoRoot ".tools\vcpkg"
    }
}
$VcpkgRoot = [System.IO.Path]::GetFullPath($VcpkgRoot)
$vcpkgGit = Join-Path $VcpkgRoot ".git"
$nativeBin = Join-Path $VcpkgRoot "installed\x64-windows\bin"
if (-not (Test-Path -LiteralPath $vcpkgGit) -or
    -not (Test-Path -LiteralPath (Join-Path $nativeBin "heif.dll")) -or
    -not (Test-Path -LiteralPath (Join-Path $nativeBin "libde265.dll"))) {
    throw "Pinned x64-windows vcpkg dependencies were not found at $VcpkgRoot."
}

$expectedBaseline = [string](
    Get-Content -LiteralPath $vcpkgManifestPath -Raw |
        ConvertFrom-Json
).'builtin-baseline'
$observedBaseline = (& git -C $VcpkgRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $observedBaseline -cne $expectedBaseline) {
    throw "vcpkg baseline mismatch. Expected $expectedBaseline; found $observedBaseline."
}

$savedEnvironment = @{
    VCPKG_ROOT = $env:VCPKG_ROOT
    VCPKG_DEFAULT_TRIPLET = $env:VCPKG_DEFAULT_TRIPLET
    VCPKG_DEFAULT_HOST_TRIPLET = $env:VCPKG_DEFAULT_HOST_TRIPLET
    VCPKGRS_TRIPLET = $env:VCPKGRS_TRIPLET
    VCPKGRS_DYNAMIC = $env:VCPKGRS_DYNAMIC
    Path = $env:Path
}

try {
    $env:VCPKG_ROOT = $VcpkgRoot
    $env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
    $env:VCPKG_DEFAULT_HOST_TRIPLET = "x64-windows"
    $env:VCPKGRS_TRIPLET = "x64-windows"
    $env:VCPKGRS_DYNAMIC = "1"
    $env:Path = "$nativeBin;$($savedEnvironment.Path)"

    & cargo build --locked --manifest-path $manifestPath `
        --package imgviewer-codec-helper --features heic
    if ($LASTEXITCODE -ne 0) {
        throw "HEIC-enabled helper build failed with exit code $LASTEXITCODE."
    }

    $metadata = & cargo metadata --locked --manifest-path $manifestPath `
        --format-version 1 --no-deps
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE."
    }
    $targetDirectory = [string]($metadata | ConvertFrom-Json).target_directory
    $helperSource = Join-Path $targetDirectory "debug\imgviewer-codec-helper.exe"
    $testDirectory = Join-Path $targetDirectory "debug\deps"
    $helperTarget = Join-Path $testDirectory "ImgViewer.CodecHelper.exe"
    if (-not (Test-Path -LiteralPath $helperSource)) {
        throw "Built helper was not found at $helperSource."
    }
    New-Item -ItemType Directory -Force -Path $testDirectory | Out-Null
    Copy-Item -LiteralPath $helperSource -Destination $helperTarget -Force

    & cargo test --locked --manifest-path $manifestPath --package imgviewer `
        --lib --no-default-features `
        "codec_helper::tests::real_helper_process_decodes_persistently_and_recovers_after_crash" `
        -- --exact --ignored --nocapture
    if ($LASTEXITCODE -ne 0) {
        throw "Real codec helper process test failed with exit code $LASTEXITCODE."
    }

    Write-Output (
        "PASS codec-helper-process primary=3x5 persistent=2 crash-recovery=1 " +
        "handle-release=4 helper=$helperTarget"
    )
} finally {
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable(
            [string]$entry.Key,
            [string]$entry.Value,
            [EnvironmentVariableTarget]::Process
        )
    }
}
