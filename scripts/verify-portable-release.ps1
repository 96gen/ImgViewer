#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$ArtifactPath,
    [string]$ChecksumPath,
    [string]$MetadataPath,
    [string]$ExpectedVersion,
    [string]$ExpectedCommit,
    [string]$ExpectedTag,
    [string]$ExpectedSha256,
    [switch]$RequireCleanSource,
    [switch]$SkipDllClosure,
    [switch]$RunNegativeTests,
    [ValidateSet(
        "None",
        "ChecksumMismatch",
        "MetadataVersionMismatch",
        "NativeToolsetMismatch",
        "NativeHashMismatch",
        "MissingDll",
        "MissingHelper",
        "HelperHashMismatch"
    )]
    [string]$NegativeMode = "None"
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$forbiddenCodecFilePattern =
    '^(?i:(?:lib)?(?:x265|aom|avif|dav1d|rav1e|svt[-_]?av1|kvazaar|vvenc))[^\\/]*\.(?:dll|exe)$'

function Assert-SafeChildPath {
    param(
        [Parameter(Mandatory)] [string]$Parent,
        [Parameter(Mandatory)] [string]$Child
    )

    $parentFull = [System.IO.Path]::GetFullPath($Parent).TrimEnd('\', '/') +
        [System.IO.Path]::DirectorySeparatorChar
    $childFull = [System.IO.Path]::GetFullPath($Child)
    if (-not $childFull.StartsWith($parentFull, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "Unsafe temporary path: $childFull"
    }
}

function Read-ChecksumManifest {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$ExpectedFileName
    )

    $lines = @(
        Get-Content -LiteralPath $Path |
            ForEach-Object { ([string]$_).Trim() } |
            Where-Object { $_ }
    )
    if ($lines.Count -ne 1) {
        throw "Checksum manifest must contain exactly one non-empty line."
    }
    if ($lines[0] -notmatch '^(?<hash>[0-9A-Fa-f]{64})[ \t]+\*?(?<name>[^\\/]+)$') {
        throw "Checksum manifest is not in SHA256SUMS format."
    }
    if ($Matches.name -cne $ExpectedFileName) {
        throw "Checksum manifest names '$($Matches.name)', expected '$ExpectedFileName'."
    }

    return $Matches.hash.ToLowerInvariant()
}

function Assert-ArtifactChecksum {
    param(
        [Parameter(Mandatory)] [string]$Artifact,
        [Parameter(Mandatory)] [string]$Manifest,
        [string]$IndependentExpectedHash
    )

    $fileName = [System.IO.Path]::GetFileName($Artifact)
    $manifestHash = Read-ChecksumManifest -Path $Manifest -ExpectedFileName $fileName
    $actualHash = (Get-FileHash -LiteralPath $Artifact -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($manifestHash -cne $actualHash) {
        throw "SHA-256 mismatch for $fileName. Manifest=$manifestHash Actual=$actualHash"
    }
    if ($IndependentExpectedHash) {
        if ($IndependentExpectedHash -notmatch '^[0-9A-Fa-f]{64}$') {
            throw "ExpectedSha256 must contain exactly 64 hexadecimal characters."
        }
        if ($actualHash -cne $IndependentExpectedHash.ToLowerInvariant()) {
            throw "Independent SHA-256 does not match. Expected=$($IndependentExpectedHash.ToLowerInvariant()) Actual=$actualHash"
        }
    }

    return $actualHash
}

function Assert-ZipEntrySafety {
    param(
        [Parameter(Mandatory)] [System.IO.Compression.ZipArchive]$Archive,
        [Parameter(Mandatory)] [string]$ExpectedRoot
    )

    $maxEntries = 256
    $maxEntryBytes = 256MB
    $maxTotalBytes = 512MB
    $maxCompressionRatio = 1000
    if ($Archive.Entries.Count -gt $maxEntries) {
        throw "ZIP contains too many entries: $($Archive.Entries.Count) > $maxEntries"
    }

    [long]$totalBytes = 0
    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $Archive.Entries) {
        $name = $entry.FullName.Replace('\', '/')
        $segments = @($name.Split('/') | Where-Object { $_ })
        if (-not $name -or $name.StartsWith('/') -or $name.Contains(':') -or
            $name.Contains('//') -or $segments.Count -eq 0) {
            throw "ZIP contains an unsafe entry path: $name"
        }
        foreach ($segment in $segments) {
            if ($segment -in @(".", "..") -or $segment -match '[\x00-\x1F]' -or
                $segment.Length -gt 255 -or $segment.EndsWith(' ') -or $segment.EndsWith('.')) {
                throw "ZIP contains an unsafe Windows path segment: $name"
            }
            $deviceName = $segment.Split('.')[0]
            if ($deviceName -match '^(?i:CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])$') {
                throw "ZIP contains a reserved Windows device name: $name"
            }
        }
        if (-not $seen.Add($name)) {
            throw "ZIP contains a duplicate or case-colliding entry: $name"
        }
        $root = $name.Split('/')[0]
        if ($root -cne $ExpectedRoot) {
            throw "ZIP entry is outside the expected root '$ExpectedRoot': $name"
        }
        if ($entry.Length -gt $maxEntryBytes) {
            throw "ZIP entry exceeds the uncompressed size limit: $name"
        }
        $totalBytes += $entry.Length
        if ($totalBytes -gt $maxTotalBytes) {
            throw "ZIP exceeds the total uncompressed size limit."
        }
        if ($entry.Length -gt 0 -and
            ($entry.CompressedLength -eq 0 -or
                $entry.Length -gt ($entry.CompressedLength * $maxCompressionRatio))) {
            throw "ZIP entry exceeds the compression-ratio limit: $name"
        }
    }
}

function Assert-Metadata {
    param(
        [Parameter(Mandatory)] [psobject]$Metadata,
        [Parameter(Mandatory)] [string]$ArtifactFileName,
        [Parameter(Mandatory)] [string]$Version,
        [string]$Commit,
        [string]$Tag,
        [switch]$CleanSource
    )

    if ([int]$Metadata.schemaVersion -ne 2) {
        throw "Unsupported BUILD_METADATA schemaVersion: $($Metadata.schemaVersion)"
    }
    if ([string]$Metadata.application.name -cne "ImgViewer") {
        throw "BUILD_METADATA application name is not ImgViewer."
    }
    if ([string]$Metadata.application.version -cne $Version) {
        throw "BUILD_METADATA version '$($Metadata.application.version)' does not match '$Version'."
    }
    if ([string]$Metadata.artifact.fileName -cne $ArtifactFileName) {
        throw "BUILD_METADATA artifact filename does not match '$ArtifactFileName'."
    }
    if ($Commit -and ([string]$Metadata.source.commit -cne $Commit.ToLowerInvariant())) {
        throw "BUILD_METADATA source commit '$($Metadata.source.commit)' does not match '$($Commit.ToLowerInvariant())'."
    }
    if ($Tag -and ([string]$Metadata.source.tag -cne $Tag)) {
        throw "BUILD_METADATA source tag '$($Metadata.source.tag)' does not match '$Tag'."
    }
    if ($CleanSource -and [bool]$Metadata.source.dirty) {
        throw "Release metadata reports a dirty source tree."
    }
    if ([string]$Metadata.native.platformToolset -cne "v143") {
        throw "BUILD_METADATA native platform toolset is not the pinned v143."
    }
    if ($CleanSource) {
        $expectedNativeProvenance = [ordered]@{
            vcpkgReleaseTag = "2026.05.25"
            vcpkgTagObject = "baddcee32f29086c2c1c1f002df5078e371f7934"
            vcpkgBuiltinBaseline = "d015e31e90838a4c9dfa3eed45979bc70d9357fc"
            vcpkgCheckout = "d015e31e90838a4c9dfa3eed45979bc70d9357fc"
            vcpkgToolReleaseTag = "2026-04-08"
            vcpkgToolSha256 = "f1edbf3a39de350e2bb065214fdc057111aa87a2e2ed9a7dcb8ddc86e17751b9"
        }
        foreach ($entry in $expectedNativeProvenance.GetEnumerator()) {
            if ([string]$Metadata.native.($entry.Key) -cne [string]$entry.Value) {
                throw "BUILD_METADATA native provenance '$($entry.Key)' does not match the release contract."
            }
        }
        if ([string]::IsNullOrWhiteSpace([string]$Metadata.native.vcpkgToolVersion)) {
            throw "BUILD_METADATA does not report the pinned vcpkg-tool version."
        }
    }
}

function Assert-ExecutablePayloadHashes {
    param(
        [Parameter(Mandatory)] [psobject]$Metadata,
        [Parameter(Mandatory)] [string]$StageDirectory
    )

    $executables = @($Metadata.executables)
    if ($executables.Count -ne 2) {
        throw "BUILD_METADATA must describe exactly the main and codec-helper executables."
    }

    $expectedFiles = [ordered]@{
        "main" = "ImgViewer.exe"
        "codec-helper" = "ImgViewer.CodecHelper.exe"
    }
    $seenRoles = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::Ordinal
    )
    $protocolVersion = $null
    foreach ($item in $executables) {
        $role = [string]$item.role
        if (-not $expectedFiles.Contains($role) -or -not $seenRoles.Add($role)) {
            throw "BUILD_METADATA contains an invalid or duplicate executable role: $role"
        }
        $fileName = [string]$item.fileName
        if ($fileName -cne [string]$expectedFiles[$role]) {
            throw "BUILD_METADATA executable '$role' must name '$($expectedFiles[$role])'."
        }
        $itemProtocolVersion = [int]$item.protocolVersion
        if ($itemProtocolVersion -lt 1) {
            throw "BUILD_METADATA executable '$role' has an invalid protocol version."
        }
        if ($null -eq $protocolVersion) {
            $protocolVersion = $itemProtocolVersion
        } elseif ($itemProtocolVersion -ne $protocolVersion) {
            throw "BUILD_METADATA executables disagree on the codec helper protocol version."
        }

        $expectedHash = ([string]$item.sha256).ToUpperInvariant()
        if ($expectedHash -notmatch '^[0-9A-F]{64}$') {
            throw "BUILD_METADATA contains an invalid executable SHA-256 for $fileName."
        }
        $payloadPath = Join-Path $StageDirectory $fileName
        if (-not (Test-Path -LiteralPath $payloadPath -PathType Leaf)) {
            throw "Executable payload named by BUILD_METADATA is missing: $fileName"
        }
        $actualHash = (Get-FileHash -LiteralPath $payloadPath -Algorithm SHA256).Hash.ToUpperInvariant()
        if ($actualHash -cne $expectedHash) {
            throw "Executable payload hash mismatch for $fileName. Metadata=$expectedHash Actual=$actualHash"
        }
    }

    foreach ($role in $expectedFiles.Keys) {
        if (-not $seenRoles.Contains($role)) {
            throw "BUILD_METADATA is missing executable role: $role"
        }
    }
}

function Assert-NativePayloadHashes {
    param(
        [Parameter(Mandatory)] [psobject]$Metadata,
        [Parameter(Mandatory)] [string]$StageDirectory
    )

    $codecs = @($Metadata.native.codecs)
    $runtimes = @($Metadata.native.msvcRuntime)
    if ($codecs.Count -lt 2) {
        throw "BUILD_METADATA must describe libheif and libde265."
    }
    if ($runtimes.Count -lt 1) {
        throw "BUILD_METADATA does not describe a bundled MSVC runtime."
    }

    $seen = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($item in @($codecs) + @($runtimes)) {
        $fileName = [string]$item.fileName
        if (-not $fileName -or [System.IO.Path]::GetFileName($fileName) -cne $fileName -or
            -not $seen.Add($fileName)) {
            throw "BUILD_METADATA contains an invalid or duplicate native filename: $fileName"
        }
        $expectedHash = ([string]$item.sha256).ToUpperInvariant()
        if ($expectedHash -notmatch '^[0-9A-F]{64}$') {
            throw "BUILD_METADATA contains an invalid SHA-256 for $fileName."
        }
        $payloadPath = Join-Path $StageDirectory $fileName
        if (-not (Test-Path -LiteralPath $payloadPath -PathType Leaf)) {
            throw "Native payload named by BUILD_METADATA is missing: $fileName"
        }
        $actualHash = (Get-FileHash -LiteralPath $payloadPath -Algorithm SHA256).Hash.ToUpperInvariant()
        if ($actualHash -cne $expectedHash) {
            throw "Native payload hash mismatch for $fileName. Metadata=$expectedHash Actual=$actualHash"
        }
    }
    foreach ($required in @("heif.dll", "libde265.dll")) {
        if (-not $seen.Contains($required)) {
            throw "BUILD_METADATA is missing required native codec payload: $required"
        }
    }
}

function Invoke-DllClosure {
    param([Parameter(Mandatory)] [string]$StageDirectory)

    $resolver = Join-Path $PSScriptRoot "Resolve-NativeDependencies.ps1"
    & $resolver -StageDirectory $StageDirectory -RequireBundledMsvcRuntime | Out-Null
    $boundary = Join-Path $PSScriptRoot "Assert-CodecBinaryBoundary.ps1"
    & $boundary -StageDirectory $StageDirectory | Out-Null
}

function Assert-ExpectedFailure {
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
        throw "Negative verification '$Name' unexpectedly succeeded."
    }
    Write-Host "PASS expected-failure=$Name"
}

function Assert-NoForbiddenCodecEntries {
    param([Parameter(Mandatory)] [string[]]$EntryNames)

    $forbiddenEntries = @(
        $EntryNames | Where-Object {
            [System.IO.Path]::GetFileName($_) -match $forbiddenCodecFilePattern
        }
    )
    if ($forbiddenEntries.Count -gt 0) {
        throw "ZIP verification found a forbidden codec: $($forbiddenEntries -join ', ')"
    }
}

$ArtifactPath = [System.IO.Path]::GetFullPath($ArtifactPath)
if (-not (Test-Path -LiteralPath $ArtifactPath -PathType Leaf)) {
    throw "Portable artifact does not exist: $ArtifactPath"
}
$artifactFileName = [System.IO.Path]::GetFileName($ArtifactPath)
if ($artifactFileName -notmatch '^ImgViewer-(?<version>\d+\.\d+\.\d+(?:[-.][0-9A-Za-z.-]+)?)-windows-x64\.zip$') {
    throw "Portable artifact filename is invalid: $artifactFileName"
}
$fileVersion = $Matches.version
if (-not $ExpectedVersion) {
    $ExpectedVersion = $fileVersion
}
if ($ExpectedVersion -cne $fileVersion) {
    throw "Expected version '$ExpectedVersion' does not match artifact filename version '$fileVersion'."
}

if (-not $ChecksumPath) {
    $ChecksumPath = "$ArtifactPath.sha256"
}
$ChecksumPath = [System.IO.Path]::GetFullPath($ChecksumPath)
if (-not (Test-Path -LiteralPath $ChecksumPath -PathType Leaf)) {
    throw "Checksum manifest does not exist: $ChecksumPath"
}

if ($NegativeMode -eq "ChecksumMismatch") {
    Assert-ExpectedFailure -Name "checksum-mismatch" -Operation {
        Assert-ArtifactChecksum -Artifact $ArtifactPath -Manifest $ChecksumPath `
            -IndependentExpectedHash ("0" * 64) | Out-Null
    }
    return
}

$artifactHash = Assert-ArtifactChecksum -Artifact $ArtifactPath -Manifest $ChecksumPath `
    -IndependentExpectedHash $ExpectedSha256

$artifactRoot = [System.IO.Path]::GetFileNameWithoutExtension($artifactFileName)
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ImgViewer-verify-" + [Guid]::NewGuid().ToString("N"))
$extractRoot = Join-Path $temporaryRoot "extracted"
Assert-SafeChildPath -Parent ([System.IO.Path]::GetTempPath()) -Child $temporaryRoot
New-Item -ItemType Directory -Path $extractRoot -Force | Out-Null

Add-Type -AssemblyName System.IO.Compression.FileSystem
try {
    $archive = [System.IO.Compression.ZipFile]::OpenRead($ArtifactPath)
    try {
        Assert-ZipEntrySafety -Archive $archive -ExpectedRoot $artifactRoot
        $entryNames = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
        foreach ($requiredEntry in @(
            "$artifactRoot/ImgViewer.exe",
            "$artifactRoot/ImgViewer.CodecHelper.exe",
            "$artifactRoot/heif.dll",
            "$artifactRoot/libde265.dll",
            "$artifactRoot/LICENSE",
            "$artifactRoot/README.md",
            "$artifactRoot/THIRD_PARTY_NOTICES.md",
            "$artifactRoot/SOURCE_VERSIONS.txt",
            "$artifactRoot/BUILD_METADATA.json"
        )) {
            if ($entryNames -cnotcontains $requiredEntry) {
                throw "ZIP verification failed; missing entry: $requiredEntry"
            }
        }
        Assert-NoForbiddenCodecEntries -EntryNames $entryNames
    } finally {
        $archive.Dispose()
    }

    Expand-Archive -LiteralPath $ArtifactPath -DestinationPath $extractRoot
    $stageDirectory = Join-Path $extractRoot $artifactRoot
    $insideMetadataPath = Join-Path $stageDirectory "BUILD_METADATA.json"
    $metadata = Get-Content -LiteralPath $insideMetadataPath -Raw | ConvertFrom-Json

    $metadataVersion = $ExpectedVersion
    if ($NegativeMode -eq "MetadataVersionMismatch") {
        $metadataVersion = "999.999.999"
    }
    if ($NegativeMode -eq "MetadataVersionMismatch") {
        Assert-ExpectedFailure -Name "metadata-version-mismatch" -Operation {
            Assert-Metadata -Metadata $metadata -ArtifactFileName $artifactFileName `
                -Version $metadataVersion -Commit $ExpectedCommit -Tag $ExpectedTag `
                -CleanSource:$RequireCleanSource
        }
        return
    }
    if ($NegativeMode -eq "NativeToolsetMismatch") {
        $metadata.native.platformToolset = "v145"
        Assert-ExpectedFailure -Name "native-toolset-mismatch" -Operation {
            Assert-Metadata -Metadata $metadata -ArtifactFileName $artifactFileName `
                -Version $metadataVersion -Commit $ExpectedCommit -Tag $ExpectedTag `
                -CleanSource:$RequireCleanSource
        }
        return
    }

    Assert-Metadata -Metadata $metadata -ArtifactFileName $artifactFileName `
        -Version $metadataVersion -Commit $ExpectedCommit -Tag $ExpectedTag `
        -CleanSource:$RequireCleanSource
    Assert-ExecutablePayloadHashes -Metadata $metadata -StageDirectory $stageDirectory
    Assert-NativePayloadHashes -Metadata $metadata -StageDirectory $stageDirectory

    if ($MetadataPath) {
        $MetadataPath = [System.IO.Path]::GetFullPath($MetadataPath)
        if (-not (Test-Path -LiteralPath $MetadataPath -PathType Leaf)) {
            throw "BUILD_METADATA sidecar does not exist: $MetadataPath"
        }
        $insideHash = (Get-FileHash -LiteralPath $insideMetadataPath -Algorithm SHA256).Hash
        $outsideHash = (Get-FileHash -LiteralPath $MetadataPath -Algorithm SHA256).Hash
        if ($insideHash -cne $outsideHash) {
            throw "BUILD_METADATA sidecar differs from the copy inside the ZIP."
        }
    }

    if ($NegativeMode -eq "MissingDll") {
        if ($SkipDllClosure) {
            throw "MissingDll negative verification requires DLL closure checks."
        }
        $negativeStage = Join-Path $temporaryRoot "missing-dll"
        Copy-Item -LiteralPath $stageDirectory -Destination $negativeStage -Recurse
        Remove-Item -LiteralPath (Join-Path $negativeStage "libde265.dll") -Force
        Assert-ExpectedFailure -Name "missing-dll-closure" -Operation {
            Invoke-DllClosure -StageDirectory $negativeStage
        }
        return
    }
    if ($NegativeMode -eq "NativeHashMismatch") {
        $negativeStage = Join-Path $temporaryRoot "native-hash-mismatch"
        Copy-Item -LiteralPath $stageDirectory -Destination $negativeStage -Recurse
        $tamperedPath = Join-Path $negativeStage "heif.dll"
        $stream = [System.IO.File]::Open($tamperedPath, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write)
        try {
            $stream.WriteByte(0)
        } finally {
            $stream.Dispose()
        }
        Assert-ExpectedFailure -Name "native-hash-mismatch" -Operation {
            Assert-NativePayloadHashes -Metadata $metadata -StageDirectory $negativeStage
        }
        return
    }
    if ($NegativeMode -eq "MissingHelper") {
        $negativeStage = Join-Path $temporaryRoot "missing-helper"
        Copy-Item -LiteralPath $stageDirectory -Destination $negativeStage -Recurse
        Remove-Item -LiteralPath (
            Join-Path $negativeStage "ImgViewer.CodecHelper.exe"
        ) -Force
        Assert-ExpectedFailure -Name "missing-helper" -Operation {
            Assert-ExecutablePayloadHashes -Metadata $metadata -StageDirectory $negativeStage
        }
        return
    }
    if ($NegativeMode -eq "HelperHashMismatch") {
        $negativeStage = Join-Path $temporaryRoot "helper-hash-mismatch"
        Copy-Item -LiteralPath $stageDirectory -Destination $negativeStage -Recurse
        $tamperedPath = Join-Path $negativeStage "ImgViewer.CodecHelper.exe"
        $stream = [System.IO.File]::Open(
            $tamperedPath,
            [System.IO.FileMode]::Append,
            [System.IO.FileAccess]::Write
        )
        try {
            $stream.WriteByte(0)
        } finally {
            $stream.Dispose()
        }
        Assert-ExpectedFailure -Name "helper-hash-mismatch" -Operation {
            Assert-ExecutablePayloadHashes -Metadata $metadata -StageDirectory $negativeStage
        }
        return
    }

    if (-not $SkipDllClosure) {
        Invoke-DllClosure -StageDirectory $stageDirectory
    }

    if ($RunNegativeTests) {
        Assert-ExpectedFailure -Name "forbidden-codec-lib-prefix" -Operation {
            Assert-NoForbiddenCodecEntries -EntryNames @(
                "$artifactRoot/ImgViewer.exe",
                "$artifactRoot/libx265.dll"
            )
        }
        Assert-ExpectedFailure -Name "checksum-mismatch" -Operation {
            Assert-ArtifactChecksum -Artifact $ArtifactPath -Manifest $ChecksumPath `
                -IndependentExpectedHash ("0" * 64) | Out-Null
        }
        Assert-ExpectedFailure -Name "metadata-version-mismatch" -Operation {
            Assert-Metadata -Metadata $metadata -ArtifactFileName $artifactFileName `
                -Version "999.999.999" -Commit $ExpectedCommit -Tag $ExpectedTag `
                -CleanSource:$RequireCleanSource
        }
        $originalPlatformToolset = [string]$metadata.native.platformToolset
        $metadata.native.platformToolset = "v145"
        Assert-ExpectedFailure -Name "native-toolset-mismatch" -Operation {
            Assert-Metadata -Metadata $metadata -ArtifactFileName $artifactFileName `
                -Version $ExpectedVersion -Commit $ExpectedCommit -Tag $ExpectedTag `
                -CleanSource:$RequireCleanSource
        }
        $metadata.native.platformToolset = $originalPlatformToolset
        $negativeHashStage = Join-Path $temporaryRoot "native-hash-mismatch"
        Copy-Item -LiteralPath $stageDirectory -Destination $negativeHashStage -Recurse
        $tamperedPath = Join-Path $negativeHashStage "heif.dll"
        $stream = [System.IO.File]::Open($tamperedPath, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write)
        try {
            $stream.WriteByte(0)
        } finally {
            $stream.Dispose()
        }
        Assert-ExpectedFailure -Name "native-hash-mismatch" -Operation {
            Assert-NativePayloadHashes -Metadata $metadata -StageDirectory $negativeHashStage
        }
        $missingHelperStage = Join-Path $temporaryRoot "missing-helper"
        Copy-Item -LiteralPath $stageDirectory -Destination $missingHelperStage -Recurse
        Remove-Item -LiteralPath (
            Join-Path $missingHelperStage "ImgViewer.CodecHelper.exe"
        ) -Force
        Assert-ExpectedFailure -Name "missing-helper" -Operation {
            Assert-ExecutablePayloadHashes -Metadata $metadata -StageDirectory $missingHelperStage
        }
        $helperHashStage = Join-Path $temporaryRoot "helper-hash-mismatch"
        Copy-Item -LiteralPath $stageDirectory -Destination $helperHashStage -Recurse
        $tamperedHelper = Join-Path $helperHashStage "ImgViewer.CodecHelper.exe"
        $stream = [System.IO.File]::Open(
            $tamperedHelper,
            [System.IO.FileMode]::Append,
            [System.IO.FileAccess]::Write
        )
        try {
            $stream.WriteByte(0)
        } finally {
            $stream.Dispose()
        }
        Assert-ExpectedFailure -Name "helper-hash-mismatch" -Operation {
            Assert-ExecutablePayloadHashes -Metadata $metadata -StageDirectory $helperHashStage
        }
        if (-not $SkipDllClosure) {
            $negativeStage = Join-Path $temporaryRoot "missing-dll"
            Copy-Item -LiteralPath $stageDirectory -Destination $negativeStage -Recurse
            Remove-Item -LiteralPath (Join-Path $negativeStage "libde265.dll") -Force
            Assert-ExpectedFailure -Name "missing-dll-closure" -Operation {
                Invoke-DllClosure -StageDirectory $negativeStage
            }
        }
    }

    Write-Host "PASS portable-integrity version=$ExpectedVersion sha256=$artifactHash helper=verified dll-closure=$(-not $SkipDllClosure)"
    Write-Output $artifactHash
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Assert-SafeChildPath -Parent ([System.IO.Path]::GetTempPath()) -Child $temporaryRoot
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
