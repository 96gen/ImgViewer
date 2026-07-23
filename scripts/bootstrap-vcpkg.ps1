#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$Destination
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$VcpkgRepository = "https://github.com/microsoft/vcpkg.git"
$VcpkgTag = "2026.05.25"
$VcpkgTagObject = "baddcee32f29086c2c1c1f002df5078e371f7934"
$VcpkgCommit = "d015e31e90838a4c9dfa3eed45979bc70d9357fc"

if ([string]::IsNullOrWhiteSpace($Destination)) {
    $Destination = Join-Path (Split-Path -Parent $PSScriptRoot) ".tools\vcpkg"
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(ValueFromRemainingArguments)] [string[]]$Arguments
    )

    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed with exit code ${LASTEXITCODE}: $FilePath $($Arguments -join ' ')"
    }
}

function Get-GitRevision {
    param(
        [Parameter(Mandatory)] [string]$Repository,
        [Parameter(Mandatory)] [string]$Revision
    )

    $previousErrorAction = $ErrorActionPreference
    try {
        $ErrorActionPreference = "SilentlyContinue"
        $value = @(& git -C $Repository rev-parse $Revision 2>$null)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }

    if ($exitCode -eq 0 -and $value.Count -gt 0) {
        return ([string]$value[-1]).Trim()
    }
    return $null
}

if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
    throw "Git is required to provision the pinned vcpkg checkout."
}

$Destination = [System.IO.Path]::GetFullPath($Destination)
$parent = Split-Path -Parent $Destination
New-Item -ItemType Directory -Path $parent -Force | Out-Null

if (-not (Test-Path -LiteralPath $Destination)) {
    Write-Host "Cloning vcpkg $VcpkgTag into $Destination"
    Invoke-Checked git clone --filter=blob:none --depth 1 --no-tags $VcpkgRepository $Destination
}

if (-not (Test-Path -LiteralPath (Join-Path $Destination ".git"))) {
    throw "The vcpkg destination exists but is not a Git checkout: $Destination"
}

$head = Get-GitRevision -Repository $Destination -Revision HEAD
$hasExpectedCommit = $head -eq $VcpkgCommit
$localTagObject = Get-GitRevision -Repository $Destination -Revision "refs/tags/$VcpkgTag"
$hasExpectedTag = $localTagObject -eq $VcpkgTagObject

if (-not ($hasExpectedCommit -and $hasExpectedTag)) {
    $dirty = (& git -C $Destination status --porcelain)
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to inspect the existing vcpkg checkout: $Destination"
    }
    if ($dirty) {
        throw "The existing vcpkg checkout has local changes. Refusing to replace or reset it: $Destination"
    }

    Write-Host "Fetching pinned vcpkg tag $VcpkgTag"
    Invoke-Checked git -C $Destination fetch --force --depth 1 origin "refs/tags/${VcpkgTag}:refs/tags/${VcpkgTag}"
}

$actualTagObject = Get-GitRevision -Repository $Destination -Revision "refs/tags/$VcpkgTag"
if ($actualTagObject -ne $VcpkgTagObject) {
    throw "vcpkg tag object mismatch. Expected $VcpkgTagObject, got $actualTagObject."
}

$peeledCommit = Get-GitRevision -Repository $Destination -Revision "refs/tags/${VcpkgTag}^{}"
if ($peeledCommit -ne $VcpkgCommit) {
    throw "vcpkg tag commit mismatch. Expected $VcpkgCommit, got $peeledCommit."
}

$head = Get-GitRevision -Repository $Destination -Revision HEAD
if ($head -ne $VcpkgCommit) {
    $dirty = (& git -C $Destination status --porcelain)
    if ($dirty) {
        throw "The existing vcpkg checkout has local changes. Refusing to switch revisions: $Destination"
    }
    Invoke-Checked git -C $Destination checkout --detach $VcpkgCommit
}

$vcpkgExe = Join-Path $Destination "vcpkg.exe"
if (-not (Test-Path -LiteralPath $vcpkgExe)) {
    Write-Host "Bootstrapping vcpkg.exe"
    Invoke-Checked (Join-Path $Destination "bootstrap-vcpkg.bat") -disableMetrics
}

$finalHead = Get-GitRevision -Repository $Destination -Revision HEAD
if ($finalHead -ne $VcpkgCommit) {
    throw "Pinned vcpkg checkout verification failed. Expected $VcpkgCommit, got $finalHead."
}

Write-Host "vcpkg ready: $vcpkgExe"
Write-Output $vcpkgExe
