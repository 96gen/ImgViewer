#requires -Version 5.1

[CmdletBinding()]
param(
    [string]$OutputDirectory,
    [switch]$SkipChecks,
    [switch]$SkipNativeSmoke,
    [switch]$KeepStage
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

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

$runtimeBin = (& (Join-Path $PSScriptRoot "install-native-deps.ps1") | Select-Object -Last 1)
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
$env:VCPKG_ROOT = $vcpkgRoot
$env:VCPKG_DEFAULT_TRIPLET = "x64-windows"
$env:VCPKG_DEFAULT_HOST_TRIPLET = "x64-windows"
$env:VCPKGRS_DYNAMIC = "1"
$env:VCPKGRS_TRIPLET = "x64-windows"
$env:PATH = "$runtimeBin$([System.IO.Path]::PathSeparator)$previousPath"
$env:CI = "true"

Push-Location $repoRoot
try {
    # libheif-sys copies heif.dll into Cargo OUT_DIR. Rust caches do not notice
    # when vcpkg rebuilds that DLL, so remove only this package's stale native
    # artifacts before using the HEIC test/build gate.
    Invoke-Checked cargo clean --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") -p libheif-sys
    Invoke-Checked $pnpm.Source install --frozen-lockfile
    if (-not $SkipChecks) {
        Invoke-Checked $pnpm.Source test
        Invoke-Checked cargo clippy --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") --all-targets --no-default-features --features heic "--" "-Dwarnings"
        Invoke-Checked cargo test --manifest-path (Join-Path $repoRoot "src-tauri\Cargo.toml") --no-default-features --features heic
    }
    Invoke-Checked $pnpm.Source exec tauri build --no-bundle --features heic
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

$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$artifactName = "ImgViewer-$version-windows-x64"
$stageDirectory = Join-Path $OutputDirectory $artifactName
$zipPath = Join-Path $OutputDirectory "$artifactName.zip"
Assert-SafeChildPath -Parent $OutputDirectory -Child $stageDirectory
Assert-SafeChildPath -Parent $OutputDirectory -Child $zipPath

if (Test-Path -LiteralPath $stageDirectory) {
    Remove-Item -LiteralPath $stageDirectory -Recurse -Force
}
if (Test-Path -LiteralPath $zipPath) {
    Remove-Item -LiteralPath $zipPath -Force
}
New-Item -ItemType Directory -Path $stageDirectory | Out-Null
Copy-Item -LiteralPath $builtExe.FullName -Destination (Join-Path $stageDirectory "ImgViewer.exe")

& (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
    -StageDirectory $stageDirectory `
    -SearchDirectory $runtimeBin `
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
        Where-Object {
            $_.Name -match '^(x265|aom|avif|dav1d|rav1e|SvtAv1)[^\\/]*\.(dll|exe)$'
        }
)
if ($forbiddenCodecFiles.Count -gt 0) {
    throw "Portable package contains a forbidden HEIF/AVIF codec: $($forbiddenCodecFiles.Name -join ', ')"
}

# A second pass deliberately has no external search roots. This proves that the
# staged directory is closed over every non-system native dependency.
& (Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1") `
    -StageDirectory $stageDirectory `
    -RequireBundledMsvcRuntime | Write-Host
if ($LASTEXITCODE -ne 0) {
    throw "Final DLL dependency closure validation failed."
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
$previousErrorAction = $ErrorActionPreference
try {
    $ErrorActionPreference = "SilentlyContinue"
    $sourceRevision = @(& git -C $repoRoot rev-parse HEAD 2>$null)
    $sourceRevisionExitCode = $LASTEXITCODE
} finally {
    $ErrorActionPreference = $previousErrorAction
}
if ($sourceRevisionExitCode -ne 0 -or $sourceRevision.Count -eq 0) {
    $sourceRevision = "uncommitted source tree"
} else {
    $sourceRevision = ([string]$sourceRevision[-1]).Trim()
}
$installedPackages = @(& $vcpkgExe list "--x-install-root=$vcpkgInstallRoot")
if ($LASTEXITCODE -ne 0) {
    throw "vcpkg list failed while collecting SOURCE_VERSIONS.txt metadata."
}
$libheifVersion = ($installedPackages | Where-Object { $_ -match '^libheif:x64-windows\s+' } | Select-Object -First 1)
$libde265Version = ($installedPackages | Where-Object { $_ -match '^libde265:x64-windows\s+' } | Select-Object -First 1)
if (-not $libheifVersion -or -not $libde265Version) {
    throw "vcpkg metadata is incomplete; expected installed libheif and libde265 version rows."
}

$sourceNotice = @"
ImgViewer $version portable source notice

ImgViewer source revision: $sourceRevision
vcpkg release tag: 2026.05.25
vcpkg annotated tag object: baddcee32f29086c2c1c1f002df5078e371f7934
vcpkg checkout / builtin baseline: d015e31e90838a4c9dfa3eed45979bc70d9357fc
libheif overlay: upstream pinned port with ENABLE_PLUGIN_LOADING=OFF
$libheifVersion
$libde265Version

Corresponding upstream sources:
- ImgViewer: the source repository and revision shown above
- vcpkg ports: https://github.com/microsoft/vcpkg/tree/d015e31e90838a4c9dfa3eed45979bc70d9357fc
- libheif 1.21.2: https://github.com/strukturag/libheif/tree/v1.21.2
- libde265: https://github.com/strukturag/libde265

The source repository URL is not configured in this workspace. Obtain the
matching ImgViewer source tree from the distributor; native library source links
follow above.

The HEIF decoder libraries are dynamically linked and can be replaced with
ABI-compatible modified builds. See THIRD_PARTY_NOTICES.md and licenses\*.txt.
"@
$utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $stageDirectory "SOURCE_VERSIONS.txt"), $sourceNotice, $utf8WithoutBom)

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
        "$artifactName/heif.dll",
        "$artifactName/libde265.dll",
        "$artifactName/LICENSE",
        "$artifactName/README.md",
        "$artifactName/PERFORMANCE.md",
        "$artifactName/THIRD_PARTY_NOTICES.md",
        "$artifactName/SOURCE_VERSIONS.txt"
    )) {
        if ($entryNames -notcontains $requiredEntry) {
            throw "ZIP verification failed; missing entry: $requiredEntry"
        }
    }
    $forbiddenEntries = @(
        $entryNames | Where-Object {
            [System.IO.Path]::GetFileName($_) -match '^(x265|aom|avif|dav1d|rav1e|SvtAv1)[^\\/]*\.(dll|exe)$'
        }
    )
    if ($forbiddenEntries.Count -gt 0) {
        throw "ZIP verification found a forbidden codec: $($forbiddenEntries -join ', ')"
    }
} finally {
    $archive.Dispose()
}

if (-not $KeepStage) {
    Assert-SafeChildPath -Parent $OutputDirectory -Child $stageDirectory
    Remove-Item -LiteralPath $stageDirectory -Recurse -Force
}

Write-Host "Portable artifact ready: $zipPath"
Write-Output $zipPath
