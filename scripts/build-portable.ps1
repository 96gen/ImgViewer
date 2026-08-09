#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [switch]$SkipChecks,
    [switch]$SkipNativeSmoke,
    [switch]$KeepStage,
    [switch]$FreshNative,
    [switch]$ReleaseMode,
    [string]$ExpectedTag
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$forbiddenCodecFilePattern =
    '^(?i:(?:lib)?(?:x265|aom|avif|dav1d|rav1e|svt[-_]?av1|kvazaar|vvenc))[^\\/]*\.(?:dll|exe)$'
$forbiddenVcpkgPackagePattern =
    '^(?i:(?:lib)?x265|(?:lib)?aom|(?:lib)?avif|dav1d|rav1e|svt[-_]?av1|kvazaar|vvenc)$'
$forbiddenTestArtifactPattern = '(?i)(?:fault[-_]?helper|test[-_]?hooks?)'

function Get-Sha256Hex {
    param([Parameter(Mandatory)] [string]$Path)

    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $stream = [System.IO.File]::Open(
            [System.IO.Path]::GetFullPath($Path),
            [System.IO.FileMode]::Open,
            [System.IO.FileAccess]::Read,
            [System.IO.FileShare]::Read
        )
        try {
            return [System.BitConverter]::ToString(
                $hasher.ComputeHash($stream)
            ).Replace("-", "")
        } finally {
            $stream.Dispose()
        }
    } finally {
        $hasher.Dispose()
    }
}

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path (Split-Path -Parent $PSScriptRoot) "release"
}

if ([System.Environment]::OSVersion.Platform -ne [System.PlatformID]::Win32NT) {
    throw "The portable ImgViewer artifact can only be built on Windows x64."
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
    throw "The portable ImgViewer artifact requires an x64 Windows build host."
}

$repoRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $repoRoot "src-tauri\tauri.conf.json"
$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$version = [string]$config.version
if ($version -notmatch '^\d+\.\d+\.\d+([-.][0-9A-Za-z.-]+)?$') {
    throw "Invalid Tauri application version: $version"
}

function Get-CargoPackageVersion {
    param([Parameter(Mandatory)] [string]$ManifestPath)

    $inPackageSection = $false
    foreach ($line in Get-Content -LiteralPath $ManifestPath) {
        if ($line -match '^\s*\[(?<section>[^\]]+)\]\s*$') {
            $inPackageSection = $Matches.section -ceq "package"
            continue
        }
        if ($inPackageSection -and $line -match '^\s*version\s*=\s*"(?<version>[^"]+)"\s*$') {
            return $Matches.version
        }
    }
    throw "Cargo package version was not found in $ManifestPath"
}

function Invoke-Captured {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(ValueFromRemainingArguments)] [string[]]$Arguments,
        [switch]$AllowFailure
    )

    $previousPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "SilentlyContinue"
        $output = @(& $FilePath @Arguments 2>$null)
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }
    if ($exitCode -ne 0 -and -not $AllowFailure) {
        throw "Command failed with exit code ${exitCode}: $FilePath $($Arguments -join ' ')"
    }
    if ($exitCode -ne 0) {
        return $null
    }
    return ($output -join [Environment]::NewLine).Trim()
}

function ConvertTo-SafeRepositoryUrl {
    param([string]$RepositoryUrl)

    if ([string]::IsNullOrWhiteSpace($RepositoryUrl)) {
        return $null
    }
    if ($RepositoryUrl -match '^[A-Za-z]:[\\/]' -or
        $RepositoryUrl -match '^(\\\\|//)' -or
        $RepositoryUrl -match '^(?i:file):') {
        return $null
    }
    $absoluteUri = $null
    if ([Uri]::TryCreate($RepositoryUrl, [UriKind]::Absolute, [ref]$absoluteUri) -and
        $absoluteUri.Scheme -in @("http", "https", "ssh")) {
        $builder = [UriBuilder]::new($absoluteUri)
        $builder.UserName = ""
        $builder.Password = ""
        $builder.Query = ""
        $builder.Fragment = ""
        return $builder.Uri.AbsoluteUri.TrimEnd('/')
    }
    if ($RepositoryUrl -match '^(?:[^@/\\]+@)?(?<host>[^:/\\\s]+):(?<path>[^?#\\]+?)(?:\.git)?$') {
        $safePath = $Matches.path.TrimStart('/').TrimEnd('/')
        return "ssh://$($Matches.host)/$safePath"
    }

    # Do not put local paths, file:// URLs, query strings, or unrecognized
    # credential-bearing remote formats into a public release.
    return $null
}

function Get-MsvcRedistDirectories {
    $results = [System.Collections.Generic.List[string]]::new()
    $runtimeDirectoryName = "Microsoft.VC143.CRT"

    if ($env:VCToolsRedistDir) {
        Get-ChildItem -LiteralPath (Join-Path $env:VCToolsRedistDir "x64") `
            -Directory -Filter $runtimeDirectoryName -ErrorAction SilentlyContinue |
            ForEach-Object { $results.Add($_.FullName) }
    }

    $vswhere = $null
    if ($env:VSWHERE_EXE -and (Test-Path -LiteralPath $env:VSWHERE_EXE -PathType Leaf)) {
        $vswhere = $env:VSWHERE_EXE
    } elseif (${env:ProgramFiles(x86)}) {
        $candidate = Join-Path ${env:ProgramFiles(x86)} `
            "Microsoft Visual Studio\Installer\vswhere.exe"
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            $vswhere = $candidate
        }
    }

    if ($vswhere) {
        $installationPaths = @(
            & $vswhere -all -products * `
                -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                -property installationPath
        )
        foreach ($installationPath in $installationPaths) {
            Get-ChildItem -LiteralPath (Join-Path $installationPath "VC\Redist\MSVC") `
                -Directory -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending |
                ForEach-Object {
                    Get-ChildItem -LiteralPath (Join-Path $_.FullName "x64") `
                        -Directory -Filter $runtimeDirectoryName `
                        -ErrorAction SilentlyContinue |
                        ForEach-Object { $results.Add($_.FullName) }
                }
        }
    }

    return @(
        $results |
            Select-Object -Unique |
            Sort-Object {
                $runtimePath = Join-Path $_ "VCRUNTIME140.dll"
                if (-not (Test-Path -LiteralPath $runtimePath -PathType Leaf)) {
                    return [version]"0.0"
                }
                try {
                    return [version](
                        [Diagnostics.FileVersionInfo]::GetVersionInfo(
                            $runtimePath
                        ).FileVersion
                    )
                } catch {
                    return [version]"0.0"
                }
            } -Descending
    )
}

function Assert-NativeLibraryLoadable {
    param([Parameter(Mandatory)] [string]$LibraryPath)

    if (-not ("ImgViewerBuildNativeLoader" -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class ImgViewerBuildNativeLoader
{
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr LoadLibraryExW(
        string fileName,
        IntPtr file,
        uint flags
    );

    [DllImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static extern bool FreeLibrary(IntPtr module);
}
'@
    }

    $fullPath = [System.IO.Path]::GetFullPath($LibraryPath)
    # LOAD_WITH_ALTERED_SEARCH_PATH starts dependency lookup beside the target
    # DLL. The probe directory deliberately contains the codec closure and the
    # pinned VC143 runtime, matching the final portable application directory.
    $module = [ImgViewerBuildNativeLoader]::LoadLibraryExW(
        $fullPath,
        [IntPtr]::Zero,
        0x00000008
    )
    if ($module -eq [IntPtr]::Zero) {
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "Native test DLL failed LoadLibraryExW before Cargo tests: $([IO.Path]::GetFileName($fullPath)) (Win32 $errorCode)."
    }
    if (-not [ImgViewerBuildNativeLoader]::FreeLibrary($module)) {
        throw "Native test DLL failed FreeLibrary: $([IO.Path]::GetFileName($fullPath))."
    }
}

$packageVersion = [string](Get-Content -LiteralPath (Join-Path $repoRoot "package.json") -Raw |
    ConvertFrom-Json).version
$cargoVersion = Get-CargoPackageVersion -ManifestPath (Join-Path $repoRoot "src-tauri\Cargo.toml")
$vcpkgManifest = Get-Content -LiteralPath (Join-Path $repoRoot "vcpkg.json") -Raw | ConvertFrom-Json
$vcpkgProjectVersion = [string]$vcpkgManifest.'version-string'
$versionSources = [ordered]@{
    "src-tauri/tauri.conf.json" = $version
    "src-tauri/Cargo.toml" = $cargoVersion
    "src-tauri/crates/codec-core/Cargo.toml" = Get-CargoPackageVersion -ManifestPath (
        Join-Path $repoRoot "src-tauri\crates\codec-core\Cargo.toml"
    )
    "src-tauri/crates/codec-helper/Cargo.toml" = Get-CargoPackageVersion -ManifestPath (
        Join-Path $repoRoot "src-tauri\crates\codec-helper\Cargo.toml"
    )
    "src-tauri/crates/codec-protocol/Cargo.toml" = Get-CargoPackageVersion -ManifestPath (
        Join-Path $repoRoot "src-tauri\crates\codec-protocol\Cargo.toml"
    )
    "package.json" = $packageVersion
    "vcpkg.json" = $vcpkgProjectVersion
}
$mismatchedVersions = @(
    $versionSources.GetEnumerator() |
        Where-Object { [string]$_.Value -cne $version } |
        ForEach-Object { "$($_.Key)=$($_.Value)" }
)
if ($mismatchedVersions.Count -gt 0) {
    throw "ImgViewer version mismatch. Expected $version; found $($mismatchedVersions -join ', ')."
}

$codecProtocolSource = Get-Content -LiteralPath (
    Join-Path $repoRoot "src-tauri\crates\codec-protocol\src\lib.rs"
) -Raw
if ($codecProtocolSource -notmatch
    'pub\s+const\s+PROTOCOL_VERSION\s*:\s*u16\s*=\s*(?<version>\d+)\s*;') {
    throw "Unable to read the codec helper protocol version."
}
$codecProtocolVersion = [int]$Matches.version
if ($codecProtocolVersion -ne 3) {
    throw "The codec helper protocol version must be 3 for BUILD_METADATA schema 3."
}
if ($codecProtocolSource -notmatch
    'pub\s+const\s+CODEC_HELPER_MEMORY_LIMIT_BYTES\s*:\s*usize\s*=\s*(?<value>[\d_]+)\s*;') {
    throw "Unable to read the codec helper memory limit."
}
$codecHelperMemoryLimitBytes = [long]($Matches.value -replace '_', '')
if ($codecProtocolSource -notmatch
    'pub\s+const\s+CODEC_HELPER_DECODE_DEADLINE_MS\s*:\s*u64\s*=\s*(?<value>[\d_]+)\s*;') {
    throw "Unable to read the codec helper decode deadline."
}
$codecHelperDecodeDeadlineMs = [long]($Matches.value -replace '_', '')
if ($codecHelperMemoryLimitBytes -ne 805306368 -or
    $codecHelperDecodeDeadlineMs -ne 30000) {
    throw (
        "BUILD_METADATA schema 3 requires the production codec helper limits " +
        "805306368 bytes/30000 ms; found $codecHelperMemoryLimitBytes bytes/" +
        "$codecHelperDecodeDeadlineMs ms."
    )
}

$git = Get-Command git.exe -ErrorAction SilentlyContinue
if (-not $git) {
    $git = Get-Command git -ErrorAction SilentlyContinue
}
$sourceRevision = $null
$sourceTreeDirty = $true
$sourceRepository = $null
$sourceTag = $null
if ($git) {
    $sourceRevision = Invoke-Captured -FilePath $git.Source -Arguments @("-C", $repoRoot, "rev-parse", "--verify", "HEAD") -AllowFailure
    $gitStatus = Invoke-Captured -FilePath $git.Source -Arguments @(
        "-C", $repoRoot, "status", "--porcelain=v1", "--untracked-files=all"
    ) -AllowFailure
    if ($null -ne $gitStatus) {
        $sourceTreeDirty = -not [string]::IsNullOrWhiteSpace($gitStatus)
    }
    $rawSourceRepository = Invoke-Captured -FilePath $git.Source -Arguments @(
        "-C", $repoRoot, "remote", "get-url", "origin"
    ) -AllowFailure
    $sourceRepository = ConvertTo-SafeRepositoryUrl -RepositoryUrl $rawSourceRepository
}

if ($ReleaseMode) {
    if (-not $git -or -not $sourceRevision) {
        throw "ReleaseMode requires a committed Git source tree."
    }
    if ($sourceTreeDirty) {
        throw "ReleaseMode requires a completely clean source tree, including no untracked files."
    }
    if ([string]::IsNullOrWhiteSpace($ExpectedTag)) {
        $ExpectedTag = [string]$env:GITHUB_REF_NAME
    }
    if ($ExpectedTag -notmatch '^v\d+\.\d+\.\d+([-.][0-9A-Za-z.-]+)?$') {
        throw "ReleaseMode requires an explicit semver tag such as v$version."
    }
    if ($ExpectedTag -cne "v$version") {
        throw "Release tag '$ExpectedTag' does not match application version 'v$version'."
    }
    $tagRevision = Invoke-Captured -FilePath $git.Source -Arguments @(
        "-C", $repoRoot, "rev-parse", "--verify", "$ExpectedTag^{commit}"
    ) -AllowFailure
    if (-not $tagRevision -or $tagRevision -cne $sourceRevision) {
        throw "Release tag '$ExpectedTag' does not resolve to source commit '$sourceRevision'."
    }
    $pointingTag = Invoke-Captured -FilePath $git.Source -Arguments @(
        "-C", $repoRoot, "tag", "--points-at", "HEAD", "--list", $ExpectedTag
    ) -AllowFailure
    if ($pointingTag -cne $ExpectedTag) {
        throw "Release tag '$ExpectedTag' is not attached to HEAD."
    }
    if ($env:GITHUB_SHA -and ([string]$env:GITHUB_SHA).ToLowerInvariant() -cne $sourceRevision.ToLowerInvariant()) {
        throw "GITHUB_SHA '$env:GITHUB_SHA' does not match checked-out HEAD '$sourceRevision'."
    }
    if ($env:GITHUB_REF_TYPE -and $env:GITHUB_REF_TYPE -cne "tag") {
        throw "ReleaseMode in GitHub Actions must run from a tag ref."
    }
    if ($env:GITHUB_REF_NAME -and $env:GITHUB_REF_NAME -cne $ExpectedTag) {
        throw "GITHUB_REF_NAME '$env:GITHUB_REF_NAME' does not match '$ExpectedTag'."
    }
    $sourceTag = $ExpectedTag
} elseif ($ExpectedTag) {
    throw "ExpectedTag is only valid together with ReleaseMode."
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

function Assert-SafeChildPath {
    param(
        [Parameter(Mandatory)] [string]$Parent,
        [Parameter(Mandatory)] [string]$Child
    )

    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
    $childFull = [System.IO.Path]::GetFullPath($Child)
    if (-not $childFull.StartsWith($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to modify a path outside the output directory: $childFull"
    }
}

$pnpm = Get-Command pnpm.cmd -ErrorAction SilentlyContinue
if (-not $pnpm) {
    $pnpm = Get-Command pnpm -ErrorAction SilentlyContinue
}
if (-not $pnpm) {
    throw "pnpm is required. Install the version declared by packageManager in package.json."
}
$cargoCommand = Get-Command cargo.exe -ErrorAction Stop

$nativeInstallArguments = @{}
if ($ReleaseMode -or $FreshNative) {
    $nativeInstallArguments.FreshInstall = $true
}
$runtimeBin = (& (Join-Path $PSScriptRoot "install-native-deps.ps1") @nativeInstallArguments |
    Select-Object -Last 1)
$vcpkgRoot = Join-Path $repoRoot ".tools\vcpkg"
$vcpkgExe = Join-Path $vcpkgRoot "vcpkg.exe"
$vcpkgInstallRoot = Join-Path $vcpkgRoot "installed"

$previousVcpkgRoot = $env:VCPKG_ROOT
$previousDefaultTriplet = $env:VCPKG_DEFAULT_TRIPLET
$previousDefaultHostTriplet = $env:VCPKG_DEFAULT_HOST_TRIPLET
$previousVcpkgRsDynamic = $env:VCPKGRS_DYNAMIC
$previousVcpkgRsTriplet = $env:VCPKGRS_TRIPLET
$previousPath = $env:PATH
$previousCi = $env:CI
$msvcRuntimeDirectories = @(Get-MsvcRedistDirectories)
if ($msvcRuntimeDirectories.Count -eq 0) {
    throw "The pinned MSVC v143 x64 redistributable directory was not found before native tests."
}
$nativeTestDirectory = Join-Path $repoRoot ".tools\native-test-v143"
Assert-SafeChildPath -Parent (Join-Path $repoRoot ".tools") -Child $nativeTestDirectory
if (Test-Path -LiteralPath $nativeTestDirectory) {
    $nativeTestItem = Get-Item -LiteralPath $nativeTestDirectory -Force
    if (($nativeTestItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Refusing to replace a native test runtime reparse point: $nativeTestDirectory"
    }
    Remove-Item -LiteralPath $nativeTestDirectory -Recurse -Force
}
New-Item -ItemType Directory -Path $nativeTestDirectory | Out-Null
foreach ($nativeCodecDll in @("libde265.dll", "heif.dll")) {
    Copy-Item -LiteralPath (Join-Path $runtimeBin $nativeCodecDll) `
        -Destination (Join-Path $nativeTestDirectory $nativeCodecDll)
}
& (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
    -StageDirectory $nativeTestDirectory `
    -SearchDirectory (@($runtimeBin) + $msvcRuntimeDirectories) `
    -CopyDependencies `
    -RequireBundledMsvcRuntime | Write-Host
if ($LASTEXITCODE -ne 0) {
    throw "Unable to stage the isolated MSVC v143 native test runtime."
}
$env:VCPKG_ROOT = $vcpkgRoot
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
$env:VCPKG_DEFAULT_HOST_TRIPLET = "x64-windows"
$env:VCPKGRS_DYNAMIC = "1"
$env:VCPKGRS_TRIPLET = "x64-windows"
$env:PATH = @($nativeTestDirectory, $previousPath) -join [IO.Path]::PathSeparator
$env:CI = "true"
Write-Host "MSVC v143 test runtime search: $($msvcRuntimeDirectories -join '; ')"
foreach ($nativeTestDll in @("libde265.dll", "heif.dll")) {
    $nativeTestDllPath = Join-Path $nativeTestDirectory $nativeTestDll
    if (-not (Test-Path -LiteralPath $nativeTestDllPath -PathType Leaf)) {
        throw "Native dependency install did not produce $nativeTestDll."
    }
    $nativeTestDllHash = (Get-Sha256Hex -Path $nativeTestDllPath).ToLowerInvariant()
    Write-Host "Native test DLL: $nativeTestDll sha256=$nativeTestDllHash"
    Assert-NativeLibraryLoadable -LibraryPath $nativeTestDllPath
}
Write-Host "PASS native-test-loader dlls=2 msvc-runtime=v143 app-local=1"

Push-Location $repoRoot
try {
    if ($ReleaseMode -or $FreshNative) {
        # Release candidates and tags rebuild vcpkg from source. Never combine
        # those DLLs with a restored Cargo target tree containing old native
        # OUT_DIRs or downstream test executables.
        Invoke-Checked cargo clean --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml")
    } else {
        # Ordinary local packages keep the target cache, but libheif-sys still
        # needs invalidation because Cargo does not observe a rebuilt DLL.
        Invoke-Checked cargo clean --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") "--package" libheif-sys
    }
    Invoke-Checked $pnpm.Source install --frozen-lockfile
    & (Join-Path $PSScriptRoot "Assert-CargoFeatureBoundary.ps1") `
        -ManifestPath (Join-Path $repoRoot "src-tauri\Cargo.toml") `
        -CargoExecutable $cargoCommand.Source | Write-Host
    if (-not $SkipChecks) {
        Invoke-Checked $pnpm.Source test
        Invoke-Checked cargo clippy --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") --workspace --all-targets --no-default-features "--" "-Dwarnings"
        Invoke-Checked cargo test --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") --workspace --no-default-features
        $codecPackages = @("imgviewer-codec-core", "imgviewer-codec-helper")
        foreach ($codecPackage in $codecPackages) {
            Invoke-Checked cargo clippy --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") "--package" $codecPackage --all-targets --no-default-features --features heic,tiff "--" "-Dwarnings"
            Invoke-Checked cargo test --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") "--package" $codecPackage --no-default-features --features heic,tiff --no-run
        }
        $cargoMetadata = Invoke-Captured -FilePath $cargoCommand.Source -Arguments @(
            "metadata", "--locked", "--manifest-path",
            (Join-Path $repoRoot "src-tauri\Cargo.toml"),
            "--format-version", "1", "--no-deps"
        )
        $cargoTestDirectory = Join-Path (
            [string]($cargoMetadata | ConvertFrom-Json).target_directory
        ) "debug\deps"
        New-Item -ItemType Directory -Path $cargoTestDirectory -Force | Out-Null
        Get-ChildItem -LiteralPath $nativeTestDirectory -File -Filter "*.dll" |
            ForEach-Object {
                Copy-Item -LiteralPath $_.FullName `
                    -Destination (Join-Path $cargoTestDirectory $_.Name) -Force
            }
        Write-Host "PASS native-test-stage target=debug/deps app-local-dlls=$(@(Get-ChildItem -LiteralPath $nativeTestDirectory -File -Filter '*.dll').Count)"
        foreach ($codecPackage in $codecPackages) {
            Invoke-Checked cargo test --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") "--package" $codecPackage --no-default-features --features heic,tiff
        }
    }
    # Execute the exact CLI installed by the frozen project lock. `pnpm exec`
    # depends on pnpm's global store/index being writable and has failed on
    # otherwise valid restricted Windows environments even when `.bin` exists.
    $tauriCli = Join-Path $repoRoot "node_modules\.bin\tauri.cmd"
    if (-not (Test-Path -LiteralPath $tauriCli -PathType Leaf)) {
        throw "The locked Tauri CLI shim is missing: node_modules\\.bin\\tauri.cmd"
    }
    # The Tauri process intentionally has no isolated HEIF/TIFF features. Only
    # the private helper below links libheif and the TIFF decoder.
    Invoke-Checked $tauriCli build --no-bundle "--" "--locked"
    Invoke-Checked cargo build --locked --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") --release "--package" imgviewer-codec-helper --no-default-features --features heic,tiff
    if ($ReleaseMode) {
        $postBuildRevision = Invoke-Captured -FilePath $git.Source -Arguments @(
            "-C", $repoRoot, "rev-parse", "--verify", "HEAD"
        )
        if ($postBuildRevision -cne $sourceRevision) {
            throw "Source HEAD changed during the release build."
        }
        $postBuildStatus = Invoke-Captured -FilePath $git.Source -Arguments @(
            "-C", $repoRoot, "status", "--porcelain=v1", "--untracked-files=all"
        )
        if (-not [string]::IsNullOrWhiteSpace($postBuildStatus)) {
            throw "Release build modified the source tree (for example Cargo.lock); refusing to package stale provenance.`n$postBuildStatus"
        }
    }
} finally {
    Pop-Location
    $env:VCPKG_ROOT = $previousVcpkgRoot
    $env:VCPKG_DEFAULT_TRIPLET = $previousDefaultTriplet
    $env:VCPKG_DEFAULT_HOST_TRIPLET = $previousDefaultHostTriplet
    $env:VCPKGRS_DYNAMIC = $previousVcpkgRsDynamic
    $env:VCPKGRS_TRIPLET = $previousVcpkgRsTriplet
    $env:PATH = $previousPath
    $env:CI = $previousCi
}

$releaseDirectory = Join-Path $repoRoot "src-tauri\target\release"
$builtExe = Get-ChildItem -LiteralPath $releaseDirectory -File -Filter "*.exe" |
    Where-Object { $_.BaseName -ieq "imgviewer" } |
    Select-Object -First 1
if (-not $builtExe) {
    throw "Tauri build completed but ImgViewer.exe was not found in $releaseDirectory"
}
$builtHelper = Join-Path $releaseDirectory "imgviewer-codec-helper.exe"
if (-not (Test-Path -LiteralPath $builtHelper -PathType Leaf)) {
    throw "Codec helper build completed but imgviewer-codec-helper.exe was not found in $releaseDirectory"
}

$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$artifactName = "ImgViewer-$version-windows-x64"
$stageDirectory = Join-Path $OutputDirectory $artifactName
$zipPath = Join-Path $OutputDirectory "$artifactName.zip"
$checksumPath = "$zipPath.sha256"
$metadataPath = Join-Path $OutputDirectory "$artifactName.build.json"
$sbomPath = Join-Path $OutputDirectory "$artifactName.cdx.json"
$baseSbomPath = Join-Path $OutputDirectory "$artifactName.base.cdx.json"
Assert-SafeChildPath -Parent $OutputDirectory -Child $stageDirectory
Assert-SafeChildPath -Parent $OutputDirectory -Child $zipPath
Assert-SafeChildPath -Parent $OutputDirectory -Child $checksumPath
Assert-SafeChildPath -Parent $OutputDirectory -Child $metadataPath
Assert-SafeChildPath -Parent $OutputDirectory -Child $sbomPath
Assert-SafeChildPath -Parent $OutputDirectory -Child $baseSbomPath

if (Test-Path -LiteralPath $stageDirectory) {
    Remove-Item -LiteralPath $stageDirectory -Recurse -Force
}
foreach ($oldOutput in @($zipPath, $checksumPath, $metadataPath, $sbomPath, $baseSbomPath)) {
    if (Test-Path -LiteralPath $oldOutput) {
        Remove-Item -LiteralPath $oldOutput -Force
    }
}
New-Item -ItemType Directory -Path $stageDirectory | Out-Null
Copy-Item -LiteralPath $builtExe.FullName -Destination (Join-Path $stageDirectory "ImgViewer.exe")
Copy-Item -LiteralPath $builtHelper -Destination (
    Join-Path $stageDirectory "ImgViewer.CodecHelper.exe"
)

& (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
    -StageDirectory $stageDirectory `
    -SearchDirectory (@($runtimeBin) + $msvcRuntimeDirectories) `
    -CopyDependencies `
    -RequireBundledMsvcRuntime | Write-Host
if ($LASTEXITCODE -ne 0) {
    throw "Native dependency collection failed."
}

foreach ($requiredDll in @("heif.dll", "libde265.dll")) {
    if (-not (Test-Path -LiteralPath (Join-Path $stageDirectory $requiredDll))) {
        throw "Portable package is missing required DLL: $requiredDll"
    }
}

$forbiddenCodecFiles = @(
    Get-ChildItem -LiteralPath $stageDirectory -Recurse -File |
        Where-Object { $_.Name -match $forbiddenCodecFilePattern }
)
if ($forbiddenCodecFiles.Count -gt 0) {
    throw "Portable package contains an unapproved HEIF/AVIF codec: $($forbiddenCodecFiles.Name -join ', ')"
}
$forbiddenTestArtifacts = @(
    Get-ChildItem -LiteralPath $stageDirectory -Recurse -File |
        Where-Object { $_.Name -match $forbiddenTestArtifactPattern }
)
if ($forbiddenTestArtifacts.Count -gt 0) {
    throw "Portable package contains a fault-helper or test-hooks artifact: $($forbiddenTestArtifacts.Name -join ', ')"
}

# A second pass deliberately has no external search roots. This proves that the
# staged directory is closed over every non-system native dependency.
& (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
    -StageDirectory $stageDirectory `
    -RequireBundledMsvcRuntime | Write-Host
if ($LASTEXITCODE -ne 0) {
    throw "Final DLL dependency closure validation failed."
}
& (Join-Path $PSScriptRoot "Assert-CodecBinaryBoundary.ps1") `
    -StageDirectory $stageDirectory | Write-Host
if ($LASTEXITCODE -ne 0) {
    throw "Codec process import-boundary validation failed."
}

Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination $stageDirectory
Copy-Item -LiteralPath (Join-Path $repoRoot "README.md") -Destination $stageDirectory
Copy-Item -LiteralPath (Join-Path $repoRoot "PERFORMANCE.md") -Destination $stageDirectory
Copy-Item -LiteralPath (Join-Path $repoRoot "THIRD_PARTY_NOTICES.md") -Destination $stageDirectory
$licenseDirectory = Join-Path $stageDirectory "licenses"
New-Item -ItemType Directory -Path $licenseDirectory | Out-Null
foreach ($package in @("libheif", "libde265")) {
    $copyright = Join-Path $vcpkgInstallRoot "x64-windows\share\$package\copyright"
    if (-not (Test-Path -LiteralPath $copyright)) {
        throw "vcpkg did not install the required license text: $copyright"
    }
    Copy-Item -LiteralPath $copyright -Destination (Join-Path $licenseDirectory "$package.txt")
}
$installedPackages = @(& $vcpkgExe list "--x-install-root=$vcpkgInstallRoot")
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg list failed while collecting SOURCE_VERSIONS.txt metadata."
}
$installedPackageNames = @(
    $installedPackages |
        ForEach-Object {
            if ($_ -match '^(?<package>[^:\s]+):(?<triplet>[^\s]+)\s+') {
                [string]$Matches.package
            }
        } |
        Sort-Object -Unique
)
$forbiddenInstalledPackages = @(
    $installedPackageNames |
        Where-Object { $_ -match $forbiddenVcpkgPackagePattern }
)
if ($forbiddenInstalledPackages.Count -gt 0) {
    throw "vcpkg installed unapproved HEIF/AVIF codec packages: $($forbiddenInstalledPackages -join ', ')"
}
$libheifRow = ($installedPackages | Where-Object { $_ -match '^libheif:x64-windows\s+' } | Select-Object -First 1)
$libde265Row = ($installedPackages | Where-Object { $_ -match '^libde265:x64-windows\s+' } | Select-Object -First 1)
if (-not $libheifRow -or -not $libde265Row) {
    throw "vcpkg metadata is incomplete; expected installed libheif and libde265 version rows."
}
if ($libheifRow -notmatch '^libheif:x64-windows\s+(?<version>\S+)') {
    throw "Unable to parse the installed libheif version: $libheifRow"
}
$libheifVersion = $Matches.version
if ($libde265Row -notmatch '^libde265:x64-windows\s+(?<version>\S+)') {
    throw "Unable to parse the installed libde265 version: $libde265Row"
}
$libde265Version = $Matches.version
$vcpkgToolVersion = Invoke-Captured -FilePath $vcpkgExe -Arguments @(
    "version", "--disable-metrics"
)
$vcpkgToolSha256 = (Get-Sha256Hex -Path $vcpkgExe).ToLowerInvariant()
if ($vcpkgToolSha256 -cne "f1edbf3a39de350e2bb065214fdc057111aa87a2e2ed9a7dcb8ddc86e17751b9") {
    throw "Pinned vcpkg.exe digest changed after native dependency installation."
}

$sourceRevisionDisplay = $sourceRevision
if (-not $sourceRevisionDisplay) {
    $sourceRevisionDisplay = "uncommitted source tree"
}
$sourceState = if ($sourceTreeDirty) { "dirty" } else { "clean" }
$sourceRepositoryNotice = if ($sourceRepository) {
    "- ImgViewer repository: $sourceRepository"
} else {
    "- ImgViewer repository URL: not configured in this source tree"
}

$sourceNotice = @"
ImgViewer $version portable source notice

ImgViewer source revision: $sourceRevisionDisplay
ImgViewer source state: $sourceState
ImgViewer source tag: $sourceTag
vcpkg release tag: 2026.05.25
vcpkg annotated tag object: baddcee32f29086c2c1c1f002df5078e371f7934
vcpkg checkout / builtin baseline: d015e31e90838a4c9dfa3eed45979bc70d9357fc
vcpkg-tool release: 2026-04-08
vcpkg-tool SHA-256: $vcpkgToolSha256
$vcpkgToolVersion
MSVC platform toolset: v143
libheif overlay: upstream pinned port with ENABLE_PLUGIN_LOADING=OFF
$libheifRow
$libde265Row

Corresponding upstream sources:
$sourceRepositoryNotice
- vcpkg ports: https://github.com/microsoft/vcpkg/tree/d015e31e90838a4c9dfa3eed45979bc70d9357fc
- libheif 1.21.2: https://github.com/strukturag/libheif/tree/v1.21.2
- libde265: https://github.com/strukturag/libde265

The HEIF decoder libraries are dynamically linked and can be replaced with
ABI-compatible modified builds. See THIRD_PARTY_NOTICES.md and licenses\*.txt.
"@
$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $stageDirectory "SOURCE_VERSIONS.txt"), $sourceNotice, $utf8WithoutBom)

$nodeCommand = Get-Command node.exe -ErrorAction Stop
$rustcCommand = Get-Command rustc.exe -ErrorAction Stop
$vcpkgRevision = if ($git) {
    Invoke-Captured -FilePath $git.Source -Arguments @(
        "-C", $vcpkgRoot, "rev-parse", "--verify", "HEAD"
    ) -AllowFailure
} else {
    $null
}
$codecMetadata = @(
    [ordered]@{
        name = "libheif"
        version = $libheifVersion
        triplet = "x64-windows"
        installedRow = [string]$libheifRow
        fileName = "heif.dll"
        sha256 = Get-Sha256Hex -Path (Join-Path $stageDirectory "heif.dll")
    },
    [ordered]@{
        name = "libde265"
        version = $libde265Version
        triplet = "x64-windows"
        installedRow = [string]$libde265Row
        fileName = "libde265.dll"
        sha256 = Get-Sha256Hex -Path (Join-Path $stageDirectory "libde265.dll")
    }
)
$msvcRuntimeMetadata = @(
    Get-ChildItem -LiteralPath $stageDirectory -File |
        Where-Object { $_.Name -match '^(vcruntime|msvcp|concrt)\d[^\\/]*\.dll$' } |
        Sort-Object Name |
        ForEach-Object {
            $fileVersion = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($_.FullName).FileVersion
            [ordered]@{
                fileName = $_.Name
                fileVersion = [string]$fileVersion
                sha256 = Get-Sha256Hex -Path $_.FullName
            }
        }
)
$executableMetadata = @(
    [ordered]@{
        role = "main"
        fileName = "ImgViewer.exe"
        protocolVersion = $codecProtocolVersion
        sha256 = Get-Sha256Hex -Path (
            Join-Path $stageDirectory "ImgViewer.exe"
        )
    },
    [ordered]@{
        role = "codec-helper"
        fileName = "ImgViewer.CodecHelper.exe"
        protocolVersion = $codecProtocolVersion
        sha256 = Get-Sha256Hex -Path (
            Join-Path $stageDirectory "ImgViewer.CodecHelper.exe"
        )
    }
)
$buildMetadata = [ordered]@{
    schemaVersion = 3
    application = [ordered]@{
        name = "ImgViewer"
        version = $version
        target = "windows-x64"
    }
    executables = $executableMetadata
    codecIsolation = [ordered]@{
        helperRole = "codec-helper"
        protocolVersion = $codecProtocolVersion
        isolatedFormats = @("heif", "tiff")
        cargoFeatures = @("heic", "tiff")
        memoryLimitBytes = $codecHelperMemoryLimitBytes
        decodeDeadlineMs = $codecHelperDecodeDeadlineMs
    }
    artifact = [ordered]@{
        fileName = "$artifactName.zip"
        archiveRoot = $artifactName
    }
    source = [ordered]@{
        commit = $sourceRevision
        dirty = [bool]$sourceTreeDirty
        tag = $sourceTag
        repository = $sourceRepository
    }
    build = [ordered]@{
        releaseMode = [bool]$ReleaseMode
        createdUtc = [DateTime]::UtcNow.ToString("o")
        runnerOs = [System.Environment]::OSVersion.VersionString
        architecture = [string][System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    }
    toolchains = [ordered]@{
        powershell = [string]$PSVersionTable.PSVersion
        node = Invoke-Captured -FilePath $nodeCommand.Source -Arguments @("--version")
        pnpm = Invoke-Captured -FilePath $pnpm.Source -Arguments @("--version")
        rustc = Invoke-Captured -FilePath $rustcCommand.Source -Arguments @("--version")
        cargo = Invoke-Captured -FilePath $cargoCommand.Source -Arguments @("--version")
        tauriCli = [string](Get-Content -LiteralPath (Join-Path $repoRoot "package.json") -Raw |
            ConvertFrom-Json).devDependencies.'@tauri-apps/cli'
    }
    native = [ordered]@{
        platformToolset = "v143"
        vcpkgReleaseTag = "2026.05.25"
        vcpkgTagObject = "baddcee32f29086c2c1c1f002df5078e371f7934"
        vcpkgBuiltinBaseline = [string]$vcpkgManifest.'builtin-baseline'
        vcpkgCheckout = $vcpkgRevision
        vcpkgToolReleaseTag = "2026-04-08"
        vcpkgToolVersion = $vcpkgToolVersion
        vcpkgToolSha256 = $vcpkgToolSha256
        codecs = $codecMetadata
        msvcRuntime = $msvcRuntimeMetadata
    }
}
$buildMetadataJson = $buildMetadata | ConvertTo-Json -Depth 10
[System.IO.File]::WriteAllText(
    (Join-Path $stageDirectory "BUILD_METADATA.json"),
    $buildMetadataJson,
    $utf8WithoutBom
)
[System.IO.File]::WriteAllText($metadataPath, $buildMetadataJson, $utf8WithoutBom)

if (-not $SkipNativeSmoke) {
    $smokeArguments = @(
        "-NoProfile"
        "-ExecutionPolicy"
        "Bypass"
        "-File"
        (Join-Path $PSScriptRoot "smoke-native.ps1")
        "-Executable"
        (Join-Path $stageDirectory "ImgViewer.exe")
        "-FixtureDirectory"
        (Join-Path $repoRoot "tests\fixtures")
    )
    Invoke-Checked -FilePath "powershell.exe" -Arguments $smokeArguments
}

Compress-Archive -LiteralPath $stageDirectory -DestinationPath $zipPath -CompressionLevel Optimal

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
    $entryNames = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
    foreach ($requiredEntry in @(
        "$artifactName/ImgViewer.exe",
        "$artifactName/ImgViewer.CodecHelper.exe",
        "$artifactName/heif.dll",
        "$artifactName/libde265.dll",
        "$artifactName/LICENSE",
        "$artifactName/README.md",
        "$artifactName/PERFORMANCE.md",
        "$artifactName/THIRD_PARTY_NOTICES.md",
        "$artifactName/SOURCE_VERSIONS.txt",
        "$artifactName/BUILD_METADATA.json"
    )) {
        if ($entryNames -notcontains $requiredEntry) {
            throw "ZIP verification failed; missing entry: $requiredEntry"
        }
    }
    $forbiddenEntries = @(
        $entryNames | Where-Object {
            [System.IO.Path]::GetFileName($_) -match $forbiddenCodecFilePattern
        }
    )
    if ($forbiddenEntries.Count -gt 0) {
        throw "ZIP verification found a forbidden codec: $($forbiddenEntries -join ', ')"
    }
    $forbiddenTestEntries = @(
        $entryNames | Where-Object { $_ -match $forbiddenTestArtifactPattern }
    )
    if ($forbiddenTestEntries.Count -gt 0) {
        throw "ZIP verification found a fault-helper or test-hooks artifact: $($forbiddenTestEntries -join ', ')"
    }
} finally {
    $archive.Dispose()
}

$artifactHash = (Get-Sha256Hex -Path $zipPath).ToLowerInvariant()
$checksumLine = "$artifactHash  $([System.IO.Path]::GetFileName($zipPath))`n"
[System.IO.File]::WriteAllText($checksumPath, $checksumLine, $utf8WithoutBom)

$verifyArguments = @{
    ArtifactPath = $zipPath
    ChecksumPath = $checksumPath
    MetadataPath = $metadataPath
    ExpectedVersion = $version
}
if ($ReleaseMode) {
    $verifyArguments.ExpectedCommit = $sourceRevision
    $verifyArguments.ExpectedTag = $ExpectedTag
    $verifyArguments.RequireCleanSource = $true
}
& (Join-Path $PSScriptRoot "verify-portable-release.ps1") @verifyArguments | Write-Host

if (-not $KeepStage) {
    Assert-SafeChildPath -Parent $OutputDirectory -Child $stageDirectory
    Remove-Item -LiteralPath $stageDirectory -Recurse -Force
}

Write-Host "Portable artifact ready: $zipPath"
Write-Host "SHA-256 manifest ready: $checksumPath"
Write-Host "Build metadata ready: $metadataPath"
if ($env:GITHUB_OUTPUT) {
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "version=$version"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "artifact_name=$artifactName"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "artifact_path=$zipPath"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "checksum_path=$checksumPath"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "metadata_path=$metadataPath"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "sha256=$artifactHash"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Encoding UTF8 -Value "source_commit=$sourceRevision"
}
Write-Output $zipPath
