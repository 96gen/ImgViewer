#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$VcpkgRoot,
    [switch]$FreshInstall
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

function Assert-SafeVcpkgChildPath {
    param(
        [Parameter(Mandatory)] [string]$Parent,
        [Parameter(Mandatory)] [string]$Child
    )

    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/') +
        [System.IO.Path]::DirectorySeparatorChar
    $childFull = [System.IO.Path]::GetFullPath($Child)
    if (-not $childFull.StartsWith($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clean a path outside the pinned vcpkg checkout: $childFull"
    }
    if (Test-Path -LiteralPath $childFull) {
        $item = Get-Item -LiteralPath $childFull -Force
        if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to clean a reparse point in the vcpkg checkout: $childFull"
        }
    }
}

$installRoot = Join-Path $VcpkgRoot "installed"
$packagesRoot = Join-Path $VcpkgRoot "packages"
$buildtreesRoot = Join-Path $VcpkgRoot "buildtrees"
$downloadsRoot = Join-Path $repoRoot ".tools\vcpkg-downloads"
$overlayPorts = Join-Path $repoRoot "vcpkg-overlays\ports"
$overlayTriplets = Join-Path $repoRoot "vcpkg-triplets"
$tripletFile = Join-Path $overlayTriplets "x64-windows.cmake"
$env:VCPKG_DOWNLOADS = $downloadsRoot
New-Item -ItemType Directory -Path $downloadsRoot -Force | Out-Null
$triplet = "x64-windows"

if ($FreshInstall) {
    # Release builds may reuse source downloads because vcpkg verifies the
    # hashes declared by each pinned port. They must never reuse ignored
    # installed payloads, package trees, build trees, or binary archives.
    foreach ($directory in @($installRoot, $packagesRoot, $buildtreesRoot)) {
        Assert-SafeVcpkgChildPath -Parent $VcpkgRoot -Child $directory
        if (Test-Path -LiteralPath $directory) {
            Remove-Item -LiteralPath $directory -Recurse -Force
        }
    }
}

if (-not (Test-Path -LiteralPath (Join-Path $overlayPorts "libheif\portfile.cmake"))) {
    throw "The pinned libheif overlay port is missing: $overlayPorts"
}
if (-not (Test-Path -LiteralPath $tripletFile -PathType Leaf)) {
    throw "The pinned x64-windows overlay triplet is missing: $tripletFile"
}
$tripletSource = Get-Content -LiteralPath $tripletFile -Raw
if ($tripletSource -notmatch '(?m)^\s*set\(\s*VCPKG_PLATFORM_TOOLSET\s+v143\s*\)\s*$') {
    throw "The x64-windows overlay triplet must pin VCPKG_PLATFORM_TOOLSET to v143."
}

Write-Host "Installing pinned native dependencies for $triplet (MSVC v143)"
$installArguments = @(
    "install"
    "--triplet=$triplet"
    "--host-triplet=$triplet"
    "--x-manifest-root=$repoRoot"
    "--x-install-root=$installRoot"
    "--overlay-ports=$overlayPorts"
    "--overlay-triplets=$overlayTriplets"
    "--clean-after-build"
    "--no-print-usage"
)
if ($FreshInstall) {
    $installArguments += "--binarysource=clear"
}
& $vcpkgExe @installArguments
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
