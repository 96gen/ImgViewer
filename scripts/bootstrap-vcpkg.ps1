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
$VcpkgToolReleaseTag = "2026-04-08"
$VcpkgToolSha256 = "f1edbf3a39de350e2bb065214fdc057111aa87a2e2ed9a7dcb8ddc86e17751b9"

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
        # Scope Git's ownership exception to the pinned checkout and this one
        # invocation. This keeps sandboxed verification working without
        # weakening the user's global safe.directory policy.
        $value = @(& git -c "safe.directory=$Repository" -C $Repository rev-parse $Revision 2>$null)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorAction
    }

    if ($exitCode -eq 0 -and $value.Count -gt 0) {
        return ([string]$value[-1]).Trim()
    }
    return $null
}

function Assert-CleanGitCheckout {
    param(
        [Parameter(Mandatory)] [string]$Repository
    )

    $status = @(
        & git -c "safe.directory=$Repository" -C $Repository status `
            --porcelain=v1 `
            --untracked-files=all
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to inspect the existing vcpkg checkout: $Repository"
    }
    if ($status.Count -gt 0) {
        throw "The existing vcpkg checkout has tracked or untracked changes. Refusing to use it: $Repository"
    }
}

function Assert-VcpkgToolHash {
    param(
        [Parameter(Mandatory)] [string]$Executable
    )

    if (-not (Test-Path -LiteralPath $Executable -PathType Leaf)) {
        throw "The pinned vcpkg executable is missing: $Executable"
    }

    $actualHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -ne $VcpkgToolSha256) {
        throw @"
vcpkg.exe hash mismatch for vcpkg-tool $VcpkgToolReleaseTag.
Expected: $VcpkgToolSha256
Actual:   $actualHash
Delete the untrusted executable and run this script again to download the pinned tool.
"@
    }
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

# Never reuse a checkout merely because its HEAD and tag match. This detects
# modified tracked files and non-ignored untracked ports/scripts before any
# bootstrap or install command can execute.
Assert-CleanGitCheckout -Repository $Destination

$head = Get-GitRevision -Repository $Destination -Revision HEAD
$hasExpectedCommit = $head -eq $VcpkgCommit
$localTagObject = Get-GitRevision -Repository $Destination -Revision "refs/tags/$VcpkgTag"
$hasExpectedTag = $localTagObject -eq $VcpkgTagObject

if (-not ($hasExpectedCommit -and $hasExpectedTag)) {
    Write-Host "Fetching pinned vcpkg tag $VcpkgTag"
    Invoke-Checked git -c "safe.directory=$Destination" -C $Destination fetch --force --depth 1 origin "refs/tags/${VcpkgTag}:refs/tags/${VcpkgTag}"
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
    Invoke-Checked git -c "safe.directory=$Destination" -C $Destination checkout --detach $VcpkgCommit
}

$vcpkgExe = Join-Path $Destination "vcpkg.exe"
if (-not (Test-Path -LiteralPath $vcpkgExe)) {
    Write-Host "Bootstrapping vcpkg.exe"
    Invoke-Checked (Join-Path $Destination "bootstrap-vcpkg.bat") -disableMetrics
}

Assert-CleanGitCheckout -Repository $Destination
Assert-VcpkgToolHash -Executable $vcpkgExe

$finalHead = Get-GitRevision -Repository $Destination -Revision HEAD
if ($finalHead -ne $VcpkgCommit) {
    throw "Pinned vcpkg checkout verification failed. Expected $VcpkgCommit, got $finalHead."
}

Write-Host "vcpkg ready: $vcpkgExe"
Write-Output $vcpkgExe
