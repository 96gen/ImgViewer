#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$StageDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-DumpbinPath {
    if ($env:DUMPBIN_EXE -and (Test-Path -LiteralPath $env:DUMPBIN_EXE -PathType Leaf)) {
        return [System.IO.Path]::GetFullPath($env:DUMPBIN_EXE)
    }

    $command = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }

    $vswhereCandidates = @()
    if ($env:VSWHERE_EXE) {
        $vswhereCandidates += $env:VSWHERE_EXE
    }
    if (${env:ProgramFiles(x86)}) {
        $vswhereCandidates += (
            Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
        )
    }

    foreach ($vswhere in $vswhereCandidates | Select-Object -Unique) {
        if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
            continue
        }
        $matches = @(
            & $vswhere -latest -products * `
                -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
                -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe"
        )
        $match = $matches |
            Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } |
            Select-Object -First 1
        if ($match) {
            return [System.IO.Path]::GetFullPath($match)
        }
    }

    throw "dumpbin.exe was not found. Install the MSVC x64 tools, or set DUMPBIN_EXE."
}

function Get-ImportedDlls {
    param(
        [Parameter(Mandatory)] [string]$BinaryPath,
        [Parameter(Mandatory)] [string]$DumpbinPath
    )

    $output = @(& $DumpbinPath /nologo /dependents $BinaryPath 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "dumpbin failed for $BinaryPath with exit code $LASTEXITCODE.`n$($output -join [Environment]::NewLine)"
    }

    return @(
        $output |
            ForEach-Object {
                if ($_ -match '^\s+([A-Za-z0-9_.+-]+\.dll)\s*$') {
                    $Matches[1]
                }
            } |
            Where-Object { $_ } |
            Sort-Object -Unique
    )
}

$StageDirectory = [System.IO.Path]::GetFullPath($StageDirectory)
if (-not (Test-Path -LiteralPath $StageDirectory -PathType Container)) {
    throw "Stage directory does not exist: $StageDirectory"
}

$mainPath = Join-Path $StageDirectory "ImgViewer.exe"
$helperPath = Join-Path $StageDirectory "ImgViewer.CodecHelper.exe"
foreach ($required in @($mainPath, $helperPath)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Codec binary boundary cannot be checked; executable is missing: $required"
    }
}

$dumpbin = Get-DumpbinPath
$mainImports = @(Get-ImportedDlls -BinaryPath $mainPath -DumpbinPath $dumpbin)
$helperImports = @(Get-ImportedDlls -BinaryPath $helperPath -DumpbinPath $dumpbin)

$forbiddenMainImports = @(
    $mainImports | Where-Object { $_ -match '^(?i:heif|libde265)\.dll$' }
)
if ($forbiddenMainImports.Count -gt 0) {
    throw "ImgViewer.exe must not import native HEIF codecs: $($forbiddenMainImports -join ', ')"
}
if ($helperImports -inotcontains "heif.dll") {
    throw "ImgViewer.CodecHelper.exe must directly import heif.dll."
}

$result = [ordered]@{
    main = [ordered]@{
        fileName = "ImgViewer.exe"
        forbiddenCodecImports = @($forbiddenMainImports)
        imports = $mainImports
    }
    helper = [ordered]@{
        fileName = "ImgViewer.CodecHelper.exe"
        requiredCodecImport = "heif.dll"
        imports = $helperImports
    }
}
$result | ConvertTo-Json -Depth 4
