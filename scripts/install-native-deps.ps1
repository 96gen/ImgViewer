#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$VcpkgRoot
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($VcpkgRoot)) {
    $VcpkgRoot = Join-Path $repoRoot ".tools\vcpkg"
}
$VcpkgRoot = [System.IO.Path]::GetFullPath($VcpkgRoot)
$vcpkgExe = (& (Join-Path $PSScriptRoot "bootstrap-vcpkg.ps1") -Destination $VcpkgRoot | Select-Object -Last 1)
if (-not (Test-Path -LiteralPath $vcpkgExe)) {
    throw "vcpkg bootstrap did not produce an executable: $vcpkgExe"
}

$installRoot = Join-Path $VcpkgRoot "installed"
$downloadsRoot = Join-Path $repoRoot ".tools\vcpkg-downloads"
$overlayPorts = Join-Path $repoRoot "vcpkg-overlays\ports"
$env:VCPKG_DOWNLOADS = $downloadsRoot
New-Item -ItemType Directory -Path $downloadsRoot -Force | Out-Null
$triplet = "x64-windows"

if (-not (Test-Path -LiteralPath (Join-Path $overlayPorts "libheif\portfile.cmake"))) {
    throw "The pinned libheif overlay port is missing: $overlayPorts"
}

Write-Host "Installing pinned native dependencies for $triplet"
& $vcpkgExe install `
    "--triplet=$triplet" `
    "--host-triplet=$triplet" `
    "--x-manifest-root=$repoRoot" `
    "--x-install-root=$installRoot" `
    "--overlay-ports=$overlayPorts" `
    --clean-after-build `
    --no-print-usage
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg install failed with exit code $LASTEXITCODE."
}

$runtimeBin = Join-Path $installRoot "$triplet\bin"
$requiredDlls = @("heif.dll", "libde265.dll")
foreach ($dll in $requiredDlls) {
    if (-not (Test-Path -LiteralPath (Join-Path $runtimeBin $dll))) {
        throw "Required native runtime DLL was not installed: $dll"
    }
}

Write-Host "Native dependencies ready: $runtimeBin"
Write-Output $runtimeBin
