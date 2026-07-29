#requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string]$StageDirectory,
    [string[]]$SearchDirectory = @(),
    [switch]$CopyDependencies,
    [switch]$RequireBundledMsvcRuntime
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

function Get-DumpbinPath {
    if ($env:DUMPBIN_EXE -and (Test-Path -LiteralPath $env:DUMPBIN_EXE)) {
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
        $vswhereCandidates += (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe")
    }

    foreach ($vswhere in $vswhereCandidates | Select-Object -Unique) {
        if (-not (Test-Path -LiteralPath $vswhere)) {
            continue
        }
        $matches = @(& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find "VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe")
        $match = $matches | Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
        if ($match) {
            return [System.IO.Path]::GetFullPath($match)
        }
    }

    throw "dumpbin.exe was not found. Install Visual Studio Build Tools with the MSVC x64 tools, or set DUMPBIN_EXE."
}

function Get-MsvcRedistDirectories {
    $results = [System.Collections.Generic.List[string]]::new()
    $runtimeDirectoryName = "Microsoft.VC143.CRT"

    if ($env:VCToolsRedistDir) {
        Get-ChildItem -LiteralPath (Join-Path $env:VCToolsRedistDir "x64") -Directory -Filter $runtimeDirectoryName -ErrorAction SilentlyContinue |
            ForEach-Object { $results.Add($_.FullName) }
    }

    $vswhere = $null
    if ($env:VSWHERE_EXE -and (Test-Path -LiteralPath $env:VSWHERE_EXE)) {
        $vswhere = $env:VSWHERE_EXE
    } elseif (${env:ProgramFiles(x86)}) {
        $candidate = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
        if (Test-Path -LiteralPath $candidate) {
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
            Get-ChildItem -LiteralPath (Join-Path $installationPath "VC\Redist\MSVC") -Directory -ErrorAction SilentlyContinue |
                Sort-Object Name -Descending |
                ForEach-Object {
                    Get-ChildItem -LiteralPath (Join-Path $_.FullName "x64") -Directory -Filter $runtimeDirectoryName -ErrorAction SilentlyContinue |
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

function Test-SystemDependency {
    param([Parameter(Mandatory)] [string]$Name)

    if ($Name -match '^(api-ms-win-|ext-ms-win-)') {
        return $true
    }
    if (-not $env:SystemRoot) {
        return $false
    }

    return (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32\$Name"))
}

function Test-MsvcRuntimeName {
    param([Parameter(Mandatory)] [string]$Name)
    return $Name -match '^(vcruntime|msvcp|concrt)\d[^\\/]*\.dll$'
}

$StageDirectory = [System.IO.Path]::GetFullPath($StageDirectory)
if (-not (Test-Path -LiteralPath $StageDirectory -PathType Container)) {
    throw "Stage directory does not exist: $StageDirectory"
}

$dumpbin = Get-DumpbinPath
$externalSearch = [System.Collections.Generic.List[string]]::new()
foreach ($directory in @($SearchDirectory) + @(Get-MsvcRedistDirectories)) {
    if (-not $directory) {
        continue
    }
    $fullPath = [System.IO.Path]::GetFullPath($directory)
    if ((Test-Path -LiteralPath $fullPath -PathType Container) -and -not $externalSearch.Contains($fullPath)) {
        $externalSearch.Add($fullPath)
    }
}

$searchIndex = @{}
if ($CopyDependencies) {
    foreach ($directory in $externalSearch) {
        foreach ($file in Get-ChildItem -LiteralPath $directory -File -Filter "*.dll") {
            $key = $file.Name.ToLowerInvariant()
            if (-not $searchIndex.ContainsKey($key)) {
                $searchIndex[$key] = $file.FullName
            }
        }
    }
}

$queue = [System.Collections.Generic.Queue[string]]::new()
$visited = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)
Get-ChildItem -LiteralPath $StageDirectory -File |
    Where-Object { $_.Extension -in @('.exe', '.dll') } |
    ForEach-Object { $queue.Enqueue($_.FullName) }

if ($queue.Count -eq 0) {
    throw "No EXE or DLL files were found in the stage directory: $StageDirectory"
}

$missing = [System.Collections.Generic.List[string]]::new()
$copied = [System.Collections.Generic.List[string]]::new()
$systemDependencies = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::OrdinalIgnoreCase)

while ($queue.Count -gt 0) {
    $binary = $queue.Dequeue()
    if (-not $visited.Add($binary)) {
        continue
    }

    foreach ($dependency in Get-ImportedDlls -BinaryPath $binary -DumpbinPath $dumpbin) {
        $stagedPath = Join-Path $StageDirectory $dependency
        if (Test-Path -LiteralPath $stagedPath) {
            $queue.Enqueue($stagedPath)
            continue
        }

        $key = $dependency.ToLowerInvariant()
        if ($CopyDependencies -and $searchIndex.ContainsKey($key)) {
            Copy-Item -LiteralPath $searchIndex[$key] -Destination $stagedPath
            $copied.Add($dependency)
            $queue.Enqueue($stagedPath)
            continue
        }

        if ((Test-MsvcRuntimeName $dependency) -and $RequireBundledMsvcRuntime) {
            $missing.Add("$dependency (MSVC runtime must be bundled; install the VC++ x64 redistributable tools)")
            continue
        }

        if (Test-SystemDependency $dependency) {
            [void]$systemDependencies.Add($dependency)
            continue
        }

        $missing.Add("$dependency (imported by $([System.IO.Path]::GetFileName($binary)))")
    }
}

if ($missing.Count -gt 0) {
    $details = $missing | Sort-Object -Unique
    throw "Unresolved non-system DLL dependencies:`n - $($details -join "`n - ")"
}

$result = [ordered]@{
    stage = $StageDirectory
    inspected = $visited.Count
    copied = @($copied | Sort-Object -Unique)
    system = @($systemDependencies | Sort-Object)
}
$result | ConvertTo-Json -Depth 3
