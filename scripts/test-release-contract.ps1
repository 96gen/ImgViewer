#requires -Version 5.1

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$temporaryRoot = Join-Path (
    [System.IO.Path]::GetTempPath()
) ("ImgViewer-release-contract-" + [Guid]::NewGuid().ToString("N"))
$version = "0.0.0"
$artifactName = "ImgViewer-$version-windows-x64"
$stageDirectory = Join-Path $temporaryRoot $artifactName
$artifactPath = Join-Path $temporaryRoot "$artifactName.zip"
$checksumPath = "$artifactPath.sha256"
$metadataPath = Join-Path $temporaryRoot "$artifactName.build.json"
$baseSbomPath = Join-Path $temporaryRoot "$artifactName.base.cdx.json"
$sbomPath = Join-Path $temporaryRoot "$artifactName.cdx.json"
$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
$previousDumpbin = $env:DUMPBIN_EXE
$previousBoundaryMode = $env:IMGVIEWER_BOUNDARY_TEST_MODE

function Write-TestFile {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [string]$Content
    )

    [System.IO.File]::WriteAllText(
        (Join-Path $stageDirectory $Name),
        $Content,
        $utf8WithoutBom
    )
}

function Get-TestHash {
    param([Parameter(Mandatory)] [string]$Name)
    return (Get-FileHash -LiteralPath (
        Join-Path $stageDirectory $Name
    ) -Algorithm SHA256).Hash
}

function Assert-TestFailure {
    param(
        [Parameter(Mandatory)] [string]$Name,
        [Parameter(Mandatory)] [scriptblock]$Operation
    )

    $failed = $false
    try {
        & $Operation
    } catch {
        $failed = $true
    }
    if (-not $failed) {
        throw "Release contract negative test unexpectedly succeeded: $Name"
    }
}

New-Item -ItemType Directory -Path $stageDirectory -Force | Out-Null
try {
    Write-TestFile -Name "ImgViewer.exe" -Content "main-test-binary"
    Write-TestFile -Name "ImgViewer.CodecHelper.exe" -Content "helper-test-binary"
    Write-TestFile -Name "heif.dll" -Content "heif-test-library"
    Write-TestFile -Name "libde265.dll" -Content "libde265-test-library"
    Write-TestFile -Name "codec-shim.dll" -Content "shim-test-library"
    Write-TestFile -Name "vcruntime140.dll" -Content "msvc-test-runtime"
    Write-TestFile -Name "LICENSE" -Content "test"
    Write-TestFile -Name "README.md" -Content "test"
    Write-TestFile -Name "PERFORMANCE.md" -Content "test"
    Write-TestFile -Name "THIRD_PARTY_NOTICES.md" -Content "test"
    Write-TestFile -Name "SOURCE_VERSIONS.txt" -Content "test"

    $fakeDumpbin = Join-Path $temporaryRoot "dumpbin.cmd"
    $fakeDumpbinContent = @"
@echo off
if /I "%~nx3"=="ImgViewer.exe" (
  if /I "%IMGVIEWER_BOUNDARY_TEST_MODE%"=="main-heif" echo     heif.dll
  if /I "%IMGVIEWER_BOUNDARY_TEST_MODE%"=="main-shim-heif" echo     codec-shim.dll
  echo     KERNEL32.dll
  exit /b 0
)
if /I "%~nx3"=="ImgViewer.CodecHelper.exe" (
  if /I not "%IMGVIEWER_BOUNDARY_TEST_MODE%"=="helper-no-heif" echo     heif.dll
  echo     KERNEL32.dll
  exit /b 0
)
if /I "%~nx3"=="codec-shim.dll" (
  if /I "%IMGVIEWER_BOUNDARY_TEST_MODE%"=="main-shim-heif" echo     heif.dll
  echo     KERNEL32.dll
  exit /b 0
)
if /I "%~nx3"=="heif.dll" (
  echo     libde265.dll
  echo     KERNEL32.dll
  exit /b 0
)
if /I "%~nx3"=="libde265.dll" (
  echo     KERNEL32.dll
  exit /b 0
)
exit /b 1
"@
    [System.IO.File]::WriteAllText(
        $fakeDumpbin,
        $fakeDumpbinContent,
        $utf8WithoutBom
    )
    $env:DUMPBIN_EXE = $fakeDumpbin
    $boundaryScript = Join-Path $PSScriptRoot "Assert-CodecBinaryBoundary.ps1"
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = "valid"
    & $boundaryScript -StageDirectory $stageDirectory | Out-Null
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = "main-heif"
    Assert-TestFailure -Name "main-imports-heif" -Operation {
        & $boundaryScript -StageDirectory $stageDirectory | Out-Null
    }
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = "main-shim-heif"
    Assert-TestFailure -Name "main-reaches-heif-through-shim" -Operation {
        & $boundaryScript -StageDirectory $stageDirectory | Out-Null
    }
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = "helper-no-heif"
    Assert-TestFailure -Name "helper-does-not-import-heif" -Operation {
        & $boundaryScript -StageDirectory $stageDirectory | Out-Null
    }
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = "valid"
    Write-TestFile -Name "libx265.dll" -Content "forbidden-codec-library"
    Assert-TestFailure -Name "stage-contains-libx265" -Operation {
        & $boundaryScript -StageDirectory $stageDirectory | Out-Null
    }
    Remove-Item -LiteralPath (Join-Path $stageDirectory "libx265.dll") -Force
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = $previousBoundaryMode

    $metadata = [ordered]@{
        schemaVersion = 2
        application = [ordered]@{
            name = "ImgViewer"
            version = $version
            target = "windows-x64"
        }
        executables = @(
            [ordered]@{
                role = "main"
                fileName = "ImgViewer.exe"
                protocolVersion = 1
                sha256 = Get-TestHash -Name "ImgViewer.exe"
            },
            [ordered]@{
                role = "codec-helper"
                fileName = "ImgViewer.CodecHelper.exe"
                protocolVersion = 1
                sha256 = Get-TestHash -Name "ImgViewer.CodecHelper.exe"
            }
        )
        artifact = [ordered]@{
            fileName = "$artifactName.zip"
            archiveRoot = $artifactName
        }
        source = [ordered]@{
            commit = $null
            dirty = $false
            tag = $null
            repository = $null
        }
        build = [ordered]@{
            releaseMode = $false
            createdUtc = [DateTime]::UtcNow.ToString("o")
            runnerOs = "contract-test"
            architecture = "X64"
        }
        toolchains = [ordered]@{}
        native = [ordered]@{
            codecs = @(
                [ordered]@{
                    name = "libheif"
                    version = "1.21.2"
                    triplet = "x64-windows"
                    installedRow = "libheif:x64-windows 1.21.2"
                    fileName = "heif.dll"
                    sha256 = Get-TestHash -Name "heif.dll"
                },
                [ordered]@{
                    name = "libde265"
                    version = "1.0.18"
                    triplet = "x64-windows"
                    installedRow = "libde265:x64-windows 1.0.18"
                    fileName = "libde265.dll"
                    sha256 = Get-TestHash -Name "libde265.dll"
                }
            )
            msvcRuntime = @(
                [ordered]@{
                    fileName = "vcruntime140.dll"
                    fileVersion = "14.0.0.0"
                    sha256 = Get-TestHash -Name "vcruntime140.dll"
                }
            )
        }
    }
    $metadataJson = $metadata | ConvertTo-Json -Depth 10
    [System.IO.File]::WriteAllText(
        (Join-Path $stageDirectory "BUILD_METADATA.json"),
        $metadataJson,
        $utf8WithoutBom
    )
    [System.IO.File]::WriteAllText($metadataPath, $metadataJson, $utf8WithoutBom)

    Compress-Archive -LiteralPath $stageDirectory -DestinationPath $artifactPath
    $artifactHash = (
        Get-FileHash -LiteralPath $artifactPath -Algorithm SHA256
    ).Hash.ToLowerInvariant()
    [System.IO.File]::WriteAllText(
        $checksumPath,
        "$artifactHash  $([System.IO.Path]::GetFileName($artifactPath))`n",
        $utf8WithoutBom
    )

    $verifyScript = Join-Path $PSScriptRoot "verify-portable-release.ps1"
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -MetadataPath $metadataPath `
        -ExpectedVersion $version `
        -SkipDllClosure | Out-Null
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -ExpectedVersion $version `
        -SkipDllClosure `
        -NegativeMode MissingHelper | Out-Null
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -ExpectedVersion $version `
        -SkipDllClosure `
        -NegativeMode HelperHashMismatch | Out-Null

    $baseSbom = [ordered]@{
        bomFormat = "CycloneDX"
        specVersion = "1.6"
        serialNumber = "urn:uuid:$([Guid]::NewGuid())"
        version = 1
        metadata = [ordered]@{
            component = [ordered]@{
                type = "application"
                name = "release-contract-fixture"
                version = $version
                'bom-ref' = "pkg:generic/release-contract-fixture@$version"
            }
        }
        components = @(
            [ordered]@{
                type = "application"
                name = "imgviewer-codec-helper"
                version = $version
                'bom-ref' = "pkg:cargo/imgviewer-codec-helper@$version"
                purl = "pkg:cargo/imgviewer-codec-helper@$version"
            }
        )
        dependencies = @()
    }
    [System.IO.File]::WriteAllText(
        $baseSbomPath,
        ($baseSbom | ConvertTo-Json -Depth 10),
        $utf8WithoutBom
    )
    & (Join-Path $PSScriptRoot "add-native-sbom-components.ps1") `
        -BaseSbomPath $baseSbomPath `
        -ArtifactPath $artifactPath `
        -OutputPath $sbomPath | Out-Null

    $resultSbom = Get-Content -LiteralPath $sbomPath -Raw | ConvertFrom-Json
    $componentNames = @(
        $resultSbom.components | ForEach-Object { [string]$_.name }
    )
    foreach ($requiredName in @(
        "imgviewer",
        "imgviewer-codec-core",
        "imgviewer-codec-helper",
        "imgviewer-codec-protocol",
        "libheif",
        "libde265",
        "vcruntime140.dll"
    )) {
        if ($componentNames -cnotcontains $requiredName) {
            throw "Release contract test SBOM is missing $requiredName."
        }
    }
    $helperComponents = @(
        $resultSbom.components |
            Where-Object { [string]$_.name -ceq "imgviewer-codec-helper" }
    )
    if ($helperComponents.Count -ne 1 -or
        @($helperComponents[0].hashes).Count -ne 1 -or
        @(
            $helperComponents[0].properties |
                Where-Object {
                    [string]$_.name -ceq "imgviewer:bundled-file" -and
                    [string]$_.value -ceq "ImgViewer.CodecHelper.exe"
                }
        ).Count -ne 1) {
        throw "Release contract test SBOM did not merge helper payload evidence."
    }

    Write-Host "PASS release-contract schema=2 executables=2 helper-negative=2 import-boundary=5 sbom-required=7 helper-evidence=merged"
} finally {
    $env:DUMPBIN_EXE = $previousDumpbin
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = $previousBoundaryMode
    if (Test-Path -LiteralPath $temporaryRoot) {
        $tempPrefix = [System.IO.Path]::GetFullPath(
            [System.IO.Path]::GetTempPath()
        ).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
        $resolved = [System.IO.Path]::GetFullPath($temporaryRoot)
        if (-not $resolved.StartsWith(
                $tempPrefix,
                [System.StringComparison]::OrdinalIgnoreCase
            )) {
            throw "Refusing to remove unsafe test path: $resolved"
        }
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
