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
$wrongTiffBaseSbomPath = Join-Path $temporaryRoot "$artifactName.wrong-tiff.cdx.json"
$sbomPath = Join-Path $temporaryRoot "$artifactName.cdx.json"
$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
$previousDumpbin = $env:DUMPBIN_EXE
$previousBoundaryMode = $env:IMGVIEWER_BOUNDARY_TEST_MODE
$previousCargoBoundaryMode = $env:IMGVIEWER_CARGO_BOUNDARY_TEST_MODE

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

function Get-Sha256 {
    param([Parameter(Mandatory)] [string]$Path)

    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::OpenRead(
            [System.IO.Path]::GetFullPath($Path)
        )
        try {
            return (
                [System.BitConverter]::ToString(
                    $hasher.ComputeHash($stream)
                ).Replace("-", "")
            )
        } finally {
            $stream.Dispose()
        }
    } finally {
        $hasher.Dispose()
    }
}

function Get-TestHash {
    param([Parameter(Mandatory)] [string]$Name)
    return Get-Sha256 -Path (Join-Path $stageDirectory $Name)
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

    $fakeCargo = Join-Path $temporaryRoot "cargo.cmd"
    $fakeCargoContent = @"
@echo off
echo.%* | %SystemRoot%\System32\findstr.exe /L /C:"--package imgviewer-codec-helper" >nul
if not errorlevel 1 (
  echo imgviewer-codec-helper v0.0.0
  echo imgviewer-codec-core v0.0.0
  echo image v0.25.10
  if /I not "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="helper-no-tiff" echo image feature "tiff"
  if /I not "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="helper-no-tiff" echo tiff v0.10.3
  if /I not "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="helper-no-libheif" echo libheif-rs v2.7.0
  if /I not "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="helper-no-libheif" echo libheif-sys v5.2.0
  exit /b 0
)
echo imgviewer v0.0.0
echo imgviewer-codec-core v0.0.0
echo image v0.25.10
echo image feature "gif"
echo image feature "jpeg"
echo image feature "png"
echo image feature "webp"
if /I "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="main-tiff" echo image feature "tiff"
if /I "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="main-tiff" echo tiff v0.10.3
if /I "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="main-libheif" echo libheif-rs v2.7.0
if /I "%IMGVIEWER_CARGO_BOUNDARY_TEST_MODE%"=="main-libheif" echo libheif-sys v5.2.0
exit /b 0
"@
    [System.IO.File]::WriteAllText(
        $fakeCargo,
        $fakeCargoContent,
        $utf8WithoutBom
    )
    $cargoBoundaryScript = Join-Path $PSScriptRoot "Assert-CargoFeatureBoundary.ps1"
    $cargoManifest = Join-Path $repoRoot "src-tauri\Cargo.toml"
    $env:IMGVIEWER_CARGO_BOUNDARY_TEST_MODE = "valid"
    & $cargoBoundaryScript `
        -ManifestPath $cargoManifest `
        -CargoExecutable $fakeCargo | Out-Null
    foreach ($cargoBoundaryMode in @(
        "main-tiff",
        "main-libheif",
        "helper-no-tiff",
        "helper-no-libheif"
    )) {
        $env:IMGVIEWER_CARGO_BOUNDARY_TEST_MODE = $cargoBoundaryMode
        Assert-TestFailure -Name "cargo-boundary-$cargoBoundaryMode" -Operation {
            & $cargoBoundaryScript `
                -ManifestPath $cargoManifest `
                -CargoExecutable $fakeCargo | Out-Null
        }
    }
    $env:IMGVIEWER_CARGO_BOUNDARY_TEST_MODE = $previousCargoBoundaryMode

    $helperPackageId = "path+file:///fixture/imgviewer-codec-helper#0.0.0"
    $corePackageId = "path+file:///fixture/imgviewer-codec-core#0.0.0"
    $imagePackageId = "registry+fixture#image@0.25.10"
    $productionTiffPackageId = "registry+fixture#tiff@0.11.3"
    $devTiffPackageId = "registry+fixture#tiff@0.10.3"
    $fakeSbomCargoMetadata = [ordered]@{
        packages = @(
            [ordered]@{ name = "imgviewer-codec-helper"; version = "0.0.0"; id = $helperPackageId },
            [ordered]@{ name = "imgviewer-codec-core"; version = "0.0.0"; id = $corePackageId },
            [ordered]@{ name = "image"; version = "0.25.10"; id = $imagePackageId },
            [ordered]@{ name = "tiff"; version = "0.11.3"; id = $productionTiffPackageId },
            [ordered]@{ name = "tiff"; version = "0.10.3"; id = $devTiffPackageId }
        )
        resolve = [ordered]@{
            nodes = @(
                [ordered]@{
                    id = $helperPackageId
                    features = @("heic", "tiff")
                    deps = @(
                        [ordered]@{
                            name = "imgviewer_codec_core"
                            pkg = $corePackageId
                            dep_kinds = @([ordered]@{ kind = $null; target = $null })
                        }
                    )
                },
                [ordered]@{
                    id = $corePackageId
                    features = @("tiff")
                    deps = @(
                        [ordered]@{
                            name = "image"
                            pkg = $imagePackageId
                            dep_kinds = @([ordered]@{ kind = $null; target = $null })
                        },
                        [ordered]@{
                            name = "tiff"
                            pkg = $devTiffPackageId
                            dep_kinds = @([ordered]@{ kind = "dev"; target = $null })
                        }
                    )
                },
                [ordered]@{
                    id = $imagePackageId
                    features = @("tiff")
                    deps = @(
                        [ordered]@{
                            name = "tiff"
                            pkg = $productionTiffPackageId
                            dep_kinds = @([ordered]@{ kind = $null; target = $null })
                        }
                    )
                },
                [ordered]@{ id = $productionTiffPackageId; features = @(); deps = @() },
                [ordered]@{ id = $devTiffPackageId; features = @(); deps = @() }
            )
        }
    }
    $fakeSbomCargoMetadataPath = Join-Path $temporaryRoot "cargo-sbom-metadata.json"
    [System.IO.File]::WriteAllText(
        $fakeSbomCargoMetadataPath,
        ($fakeSbomCargoMetadata | ConvertTo-Json -Depth 10),
        $utf8WithoutBom
    )
    $fakeSbomCargo = Join-Path $temporaryRoot "cargo-sbom.cmd"
    $fakeSbomCargoContent = @"
@echo off
echo.%* | %SystemRoot%\System32\findstr.exe /L /C:"--filter-platform x86_64-pc-windows-msvc" >nul
if errorlevel 1 exit /b 64
echo.%* | %SystemRoot%\System32\findstr.exe /L /C:"--no-default-features" >nul
if errorlevel 1 exit /b 65
echo.%* | %SystemRoot%\System32\findstr.exe /L /C:"--features imgviewer-codec-helper/heic,imgviewer-codec-helper/tiff" >nul
if errorlevel 1 exit /b 66
type "%~dp0cargo-sbom-metadata.json"
exit /b 0
"@
    [System.IO.File]::WriteAllText(
        $fakeSbomCargo,
        $fakeSbomCargoContent,
        $utf8WithoutBom
    )

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
        schemaVersion = 3
        application = [ordered]@{
            name = "ImgViewer"
            version = $version
            target = "windows-x64"
        }
        executables = @(
            [ordered]@{
                role = "main"
                fileName = "ImgViewer.exe"
                protocolVersion = 3
                sha256 = Get-TestHash -Name "ImgViewer.exe"
            },
            [ordered]@{
                role = "codec-helper"
                fileName = "ImgViewer.CodecHelper.exe"
                protocolVersion = 3
                sha256 = Get-TestHash -Name "ImgViewer.CodecHelper.exe"
            }
        )
        codecIsolation = [ordered]@{
            helperRole = "codec-helper"
            protocolVersion = 3
            isolatedFormats = @("heif", "tiff")
            cargoFeatures = @("heic", "tiff")
            memoryLimitBytes = 805306368
            decodeDeadlineMs = 30000
        }
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
            platformToolset = "v143"
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
    $artifactHash = (Get-Sha256 -Path $artifactPath).ToLowerInvariant()
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
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -ExpectedVersion $version `
        -SkipDllClosure `
        -NegativeMode FaultHelperArtifact | Out-Null
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -ExpectedVersion $version `
        -SkipDllClosure `
        -NegativeMode TestHooksArtifact | Out-Null
    & $verifyScript `
        -ArtifactPath $artifactPath `
        -ChecksumPath $checksumPath `
        -ExpectedVersion $version `
        -SkipDllClosure `
        -NegativeMode NativeToolsetMismatch | Out-Null

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
            },
            [ordered]@{
                type = "library"
                name = "tiff"
                version = "0.11.3"
                'bom-ref' = "pkg:cargo/tiff@0.11.3"
                purl = "pkg:cargo/tiff@0.11.3"
            }
        )
        dependencies = @()
    }
    [System.IO.File]::WriteAllText(
        $baseSbomPath,
        ($baseSbom | ConvertTo-Json -Depth 10),
        $utf8WithoutBom
    )
    $wrongTiffBaseSbom = ($baseSbom | ConvertTo-Json -Depth 10 | ConvertFrom-Json)
    $wrongTiffComponent = @(
        $wrongTiffBaseSbom.components |
            Where-Object { [string]$_.name -ceq "tiff" }
    )[0]
    $wrongTiffComponent.version = "0.10.3"
    $wrongTiffComponent.'bom-ref' = "pkg:cargo/tiff@0.10.3"
    $wrongTiffComponent.purl = "pkg:cargo/tiff@0.10.3"
    [System.IO.File]::WriteAllText(
        $wrongTiffBaseSbomPath,
        ($wrongTiffBaseSbom | ConvertTo-Json -Depth 10),
        $utf8WithoutBom
    )
    $sbomScript = Join-Path $PSScriptRoot "add-native-sbom-components.ps1"
    Assert-TestFailure -Name "sbom-wrong-production-tiff-version" -Operation {
        & $sbomScript `
            -BaseSbomPath $wrongTiffBaseSbomPath `
            -ArtifactPath $artifactPath `
            -OutputPath (Join-Path $temporaryRoot "wrong-tiff-output.cdx.json") `
            -ManifestPath $cargoManifest `
            -CargoExecutable $fakeSbomCargo | Out-Null
    }
    & $sbomScript `
        -BaseSbomPath $baseSbomPath `
        -ArtifactPath $artifactPath `
        -OutputPath $sbomPath `
        -ManifestPath $cargoManifest `
        -CargoExecutable $fakeSbomCargo | Out-Null

    $resultSbom = Get-Content -LiteralPath $sbomPath -Raw | ConvertFrom-Json
    $componentNames = @(
        $resultSbom.components | ForEach-Object { [string]$_.name }
    )
    foreach ($requiredName in @(
        "imgviewer",
        "imgviewer-codec-core",
        "imgviewer-codec-helper",
        "imgviewer-codec-protocol",
        "tiff",
        "libheif",
        "libde265",
        "vcruntime140.dll"
    )) {
        if ($componentNames -cnotcontains $requiredName) {
            throw "Release contract test SBOM is missing $requiredName."
        }
    }
    $productionTiffComponents = @(
        $resultSbom.components |
            Where-Object {
                [string]$_.name -ceq "tiff" -and
                [string]$_.version -ceq "0.11.3" -and
                [string]$_.purl -ceq "pkg:cargo/tiff@0.11.3"
            }
    )
    if ($productionTiffComponents.Count -ne 1) {
        throw "Release contract test SBOM did not preserve the production helper tiff component."
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
    $expectedIsolationProperties = [ordered]@{
        "imgviewer:codec-isolation-helper-role" = "codec-helper"
        "imgviewer:codec-isolation-protocol-version" = "3"
        "imgviewer:codec-isolation-isolated-formats" = "heif,tiff"
        "imgviewer:codec-isolation-cargo-features" = "heic,tiff"
        "imgviewer:codec-isolation-memory-limit-bytes" = "805306368"
        "imgviewer:codec-isolation-decode-deadline-ms" = "30000"
    }
    foreach ($expectedProperty in $expectedIsolationProperties.GetEnumerator()) {
        $matches = @(
            $helperComponents[0].properties |
                Where-Object {
                    [string]$_.name -ceq [string]$expectedProperty.Key -and
                    [string]$_.value -ceq [string]$expectedProperty.Value
                }
        )
        if ($matches.Count -ne 1) {
            throw "Release contract test SBOM is missing codec isolation property: $($expectedProperty.Key)"
        }
    }

    $toolsetProperties = @(
        $resultSbom.components |
            Where-Object {
                [string]$_.name -in @("libheif", "libde265", "vcruntime140.dll")
            } |
            ForEach-Object { @($_.properties) } |
            Where-Object {
                [string]$_.name -ceq "imgviewer:msvc-platform-toolset" -and
                [string]$_.value -ceq "v143"
            }
    )
    if ($toolsetProperties.Count -ne 3) {
        throw "Release contract test SBOM did not preserve the pinned v143 toolset."
    }

    Write-Host "PASS release-contract schema=3 executables=2 helper-negative=2 test-artifact-negative=2 toolset-negative=1 import-boundary=5 cargo-graph-negative=4 sbom-required=8 sbom-negative=1 isolation-properties=6 helper-evidence=merged"
} finally {
    $env:DUMPBIN_EXE = $previousDumpbin
    $env:IMGVIEWER_BOUNDARY_TEST_MODE = $previousBoundaryMode
    $env:IMGVIEWER_CARGO_BOUNDARY_TEST_MODE = $previousCargoBoundaryMode
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
