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
$expectedHelperPaths = @()

try {
    $env:VCPKG_ROOT = $VcpkgRoot
    $env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
    $env:VCPKG_DEFAULT_HOST_TRIPLET = "x64-windows"
    $env:VCPKGRS_TRIPLET = "x64-windows"
    $env:VCPKGRS_DYNAMIC = "1"
    $env:Path = "$nativeBin;$($savedEnvironment.Path)"

    & cargo build --locked --manifest-path $manifestPath `
        --package imgviewer-codec-helper --bin imgviewer-codec-helper `
        --no-default-features --features heic,tiff
    if ($LASTEXITCODE -ne 0) {
        throw "HEIF+TIFF helper build failed with exit code $LASTEXITCODE."
    }

    & cargo build --locked --manifest-path $manifestPath `
        --package imgviewer-codec-helper --bin imgviewer-codec-fault-helper `
        --no-default-features --features test-hooks
    if ($LASTEXITCODE -ne 0) {
        throw "Test-only codec fault helper build failed with exit code $LASTEXITCODE."
    }

    $metadata = & cargo metadata --locked --manifest-path $manifestPath `
        --format-version 1 --no-deps
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed with exit code $LASTEXITCODE."
    }
    $targetDirectory = [string]($metadata | ConvertFrom-Json).target_directory
    $helperSource = Join-Path $targetDirectory "debug\imgviewer-codec-helper.exe"
    $faultHelperSource = Join-Path $targetDirectory "debug\imgviewer-codec-fault-helper.exe"
    $testDirectory = Join-Path $targetDirectory "debug\deps"
    $nativeStage = Join-Path $targetDirectory "debug\native-test-v143"
    $helperTarget = Join-Path $testDirectory "ImgViewer.CodecHelper.exe"
    $faultHelperTarget = Join-Path $testDirectory "ImgViewer.CodecFaultHelper.exe"
    if (-not (Test-Path -LiteralPath $helperSource)) {
        throw "Built helper was not found at $helperSource."
    }
    if (-not (Test-Path -LiteralPath $faultHelperSource)) {
        throw "Built fault helper was not found at $faultHelperSource."
    }
    $targetRoot = [IO.Path]::GetFullPath($targetDirectory).TrimEnd('\', '/') +
        [IO.Path]::DirectorySeparatorChar
    $nativeStageFull = [IO.Path]::GetFullPath($nativeStage)
    if (-not $nativeStageFull.StartsWith(
        $targetRoot,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Refusing to stage native test DLLs outside Cargo target: $nativeStageFull"
    }
    if (Test-Path -LiteralPath $nativeStageFull) {
        $nativeStageItem = Get-Item -LiteralPath $nativeStageFull -Force
        if (($nativeStageItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "Refusing to replace a native test runtime reparse point: $nativeStageFull"
        }
        Remove-Item -LiteralPath $nativeStageFull -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $nativeStageFull | Out-Null
    foreach ($nativeCodecDll in @("heif.dll", "libde265.dll")) {
        Copy-Item -LiteralPath (Join-Path $nativeBin $nativeCodecDll) `
            -Destination (Join-Path $nativeStageFull $nativeCodecDll)
    }
    & (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
        -StageDirectory $nativeStageFull `
        -SearchDirectory $nativeBin `
        -CopyDependencies `
        -RequireBundledMsvcRuntime | Write-Host
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to stage the app-local MSVC v143 helper test runtime."
    }

    New-Item -ItemType Directory -Force -Path $testDirectory | Out-Null
    Get-ChildItem -LiteralPath $nativeStageFull -File -Filter "*.dll" |
        ForEach-Object {
            Copy-Item -LiteralPath $_.FullName `
                -Destination (Join-Path $testDirectory $_.Name) -Force
    }
    Copy-Item -LiteralPath $helperSource -Destination $helperTarget -Force
    Copy-Item -LiteralPath $faultHelperSource -Destination $faultHelperTarget -Force

    $processTests = @(
        "codec_helper::tests::real_helper_process_decodes_persistently_and_recovers_after_crash",
        "codec_helper::tests::real_fault_helper_hang_times_out_once_then_recovers_lazily",
        "codec_helper::tests::real_fault_helper_job_oom_crashes_once_then_recovers_lazily"
    )
    foreach ($processTest in $processTests) {
        & cargo test --locked --manifest-path $manifestPath --package imgviewer `
            --lib --no-default-features $processTest `
            -- --exact --ignored --nocapture
        if ($LASTEXITCODE -ne 0) {
            throw "Real codec helper process test '$processTest' failed with exit code $LASTEXITCODE."
        }
    }

    $expectedHelperPaths = @(
        [IO.Path]::GetFullPath($helperTarget),
        [IO.Path]::GetFullPath($faultHelperTarget)
    )
    $orphanProcesses = @(
        Get-CimInstance Win32_Process -ErrorAction Stop |
            Where-Object {
                -not [string]::IsNullOrWhiteSpace([string]$_.ExecutablePath) -and
                $expectedHelperPaths -icontains [IO.Path]::GetFullPath(
                    [string]$_.ExecutablePath
                )
            }
    )
    if ($orphanProcesses.Count -gt 0) {
        $orphanIds = @($orphanProcesses | ForEach-Object { [int]$_.ProcessId })
        foreach ($orphanId in $orphanIds) {
            Stop-Process -Id $orphanId -Force -ErrorAction SilentlyContinue
        }
        throw "Codec helper process tests left orphan PIDs: $($orphanIds -join ', ')."
    }

    Write-Output (
        "PASS codec-helper-process formats=heif,tiff persistent=1 crash-restarts=20 " +
        "hang-recovery=1 oom-recovery=1 handle-release=verified orphan=absent"
    )
} finally {
    # A failed ignored process test can leave its deliberately hung/crashed
    # helper alive. Always clean only the exact staged helper paths before
    # restoring the caller's environment; never match by process name alone.
    if (@($expectedHelperPaths).Count -gt 0) {
        @(
            Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
                Where-Object {
                    -not [string]::IsNullOrWhiteSpace([string]$_.ExecutablePath) -and
                    $expectedHelperPaths -icontains [IO.Path]::GetFullPath(
                        [string]$_.ExecutablePath
                    )
                }
        ) | ForEach-Object {
            Stop-Process -Id ([int]$_.ProcessId) -Force -ErrorAction SilentlyContinue
        }
    }
    foreach ($entry in $savedEnvironment.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable(
            [string]$entry.Key,
            [string]$entry.Value,
            [EnvironmentVariableTarget]::Process
        )
    }
}
