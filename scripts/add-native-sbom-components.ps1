#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$BaseSbomPath,
    [Parameter(Mandatory)] [string]$ArtifactPath,
    [Parameter(Mandatory)] [string]$OutputPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Add-ComponentIfMissing {
    param(
        [Parameter(Mandatory)] [AllowEmptyCollection()] [System.Collections.ArrayList]$Components,
        [Parameter(Mandatory)] [psobject]$Component
    )

    foreach ($existing in $Components) {
        if ([string]$existing.'bom-ref' -ceq [string]$Component.'bom-ref') {
            return
        }
    }
    [void]$Components.Add($Component)
}

function Get-VerifiedNativeHash {
    param(
        [Parameter(Mandatory)] [string]$PayloadRoot,
        [Parameter(Mandatory)] [psobject]$Item
    )

    $fileName = [string]$Item.fileName
    if (-not $fileName -or [System.IO.Path]::GetFileName($fileName) -cne $fileName) {
        throw "BUILD_METADATA contains an invalid native filename: $fileName"
    }
    $metadataHash = ([string]$Item.sha256).ToUpperInvariant()
    if ($metadataHash -notmatch '^[0-9A-F]{64}$') {
        throw "BUILD_METADATA contains an invalid native hash for $fileName"
    }
    $payloadPath = Join-Path $PayloadRoot $fileName
    if (-not (Test-Path -LiteralPath $payloadPath -PathType Leaf)) {
        throw "Native file named by BUILD_METADATA is missing from the ZIP: $fileName"
    }
    $actualHash = (Get-FileHash -LiteralPath $payloadPath -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($actualHash -cne $metadataHash) {
        throw "Native file hash does not match BUILD_METADATA: $fileName"
    }
    return $actualHash
}

$BaseSbomPath = [System.IO.Path]::GetFullPath($BaseSbomPath)
$ArtifactPath = [System.IO.Path]::GetFullPath($ArtifactPath)
$OutputPath = [System.IO.Path]::GetFullPath($OutputPath)
foreach ($required in @($BaseSbomPath, $ArtifactPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required SBOM input does not exist: $required"
    }
}

$bom = Get-Content -LiteralPath $BaseSbomPath -Raw | ConvertFrom-Json
if ([string]$bom.bomFormat -cne "CycloneDX") {
    throw "Base SBOM is not a CycloneDX document."
}

$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("ImgViewer-sbom-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporaryRoot -Force | Out-Null
try {
    Expand-Archive -LiteralPath $ArtifactPath -DestinationPath $temporaryRoot
    $metadataFile = @(
        Get-ChildItem -LiteralPath $temporaryRoot -Recurse -File -Filter "BUILD_METADATA.json"
    )
    if ($metadataFile.Count -ne 1) {
        throw "Expected exactly one BUILD_METADATA.json inside the portable ZIP."
    }
    $metadata = Get-Content -LiteralPath $metadataFile[0].FullName -Raw | ConvertFrom-Json
    $payloadRoot = Split-Path -Parent $metadataFile[0].FullName

    $components = [System.Collections.ArrayList]::new()
    foreach ($component in @($bom.components)) {
        if ($null -ne $component) {
            [void]$components.Add($component)
        }
    }

    $nativeRefs = [System.Collections.Generic.List[string]]::new()
    foreach ($codec in @($metadata.native.codecs)) {
        $name = [string]$codec.name
        $version = [string]$codec.version
        $bomRef = "pkg:generic/${name}@${version}?arch=x86_64&platform=windows"
        $nativeRefs.Add($bomRef)
        $codecHash = Get-VerifiedNativeHash -PayloadRoot $payloadRoot -Item $codec
        $component = [pscustomobject][ordered]@{
            type = "library"
            name = $name
            version = $version
            scope = "required"
            'bom-ref' = $bomRef
            purl = $bomRef
            hashes = @([pscustomobject]@{ alg = "SHA-256"; content = $codecHash })
            properties = @(
                [pscustomobject]@{ name = "imgviewer:vcpkg-triplet"; value = [string]$codec.triplet },
                [pscustomobject]@{ name = "imgviewer:bundled-file"; value = [string]$codec.fileName },
                [pscustomobject]@{ name = "imgviewer:vcpkg-port-row"; value = [string]$codec.installedRow }
            )
        }
        Add-ComponentIfMissing -Components $components -Component $component
    }

    foreach ($runtime in @($metadata.native.msvcRuntime)) {
        $name = [string]$runtime.fileName
        $version = [string]$runtime.fileVersion
        if (-not $version) {
            $version = "unknown"
        }
        $hash = Get-VerifiedNativeHash -PayloadRoot $payloadRoot -Item $runtime
        $bomRef = "pkg:generic/microsoft/${name}@${version}?arch=x86_64&platform=windows"
        $nativeRefs.Add($bomRef)
        $component = [pscustomobject][ordered]@{
            type = "library"
            name = $name
            version = $version
            scope = "required"
            'bom-ref' = $bomRef
            purl = $bomRef
            hashes = @([pscustomobject]@{ alg = "SHA-256"; content = $hash })
            properties = @(
                [pscustomobject]@{ name = "imgviewer:distribution"; value = "bundled-msvc-runtime" },
                [pscustomobject]@{ name = "imgviewer:architecture"; value = "x86_64" }
            )
        }
        Add-ComponentIfMissing -Components $components -Component $component
    }
    $bom.components = @($components)

    if ($bom.metadata -and $bom.metadata.component -and $bom.metadata.component.'bom-ref') {
        $rootRef = [string]$bom.metadata.component.'bom-ref'
        $dependencies = [System.Collections.ArrayList]::new()
        $rootDependency = $null
        foreach ($dependency in @($bom.dependencies)) {
            if ($null -eq $dependency) {
                continue
            }
            if ([string]$dependency.ref -ceq $rootRef) {
                $rootDependency = $dependency
            }
            [void]$dependencies.Add($dependency)
        }
        if (-not $rootDependency) {
            $rootDependency = [pscustomobject][ordered]@{ ref = $rootRef; dependsOn = @() }
            [void]$dependencies.Add($rootDependency)
        }
        $dependsOn = [System.Collections.Generic.List[string]]::new()
        foreach ($reference in @($rootDependency.dependsOn) + @($nativeRefs)) {
            if ($reference -and -not $dependsOn.Contains([string]$reference)) {
                $dependsOn.Add([string]$reference)
            }
        }
        $rootDependency.dependsOn = @($dependsOn)
        $bom.dependencies = @($dependencies)
    }

    $outputDirectory = Split-Path -Parent $OutputPath
    if ($outputDirectory) {
        New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
    }
    $utf8WithoutBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(
        $OutputPath,
        ($bom | ConvertTo-Json -Depth 100),
        $utf8WithoutBom
    )

    $roundTrip = Get-Content -LiteralPath $OutputPath -Raw | ConvertFrom-Json
    $names = @($roundTrip.components | ForEach-Object { [string]$_.name })
    foreach ($requiredName in @("libheif", "libde265")) {
        if ($names -cnotcontains $requiredName) {
            throw "Merged CycloneDX SBOM is missing native component: $requiredName"
        }
    }
    Write-Host "CycloneDX SBOM enriched with ImgViewer native runtime components: $OutputPath"
    Write-Output $OutputPath
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        $tempPrefix = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath()).TrimEnd('\', '/') +
            [System.IO.Path]::DirectorySeparatorChar
        $resolved = [System.IO.Path]::GetFullPath($temporaryRoot)
        if (-not $resolved.StartsWith($tempPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to remove unsafe temporary path: $resolved"
        }
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
