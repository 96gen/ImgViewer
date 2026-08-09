[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string]$ReleaseTag,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedCommit
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$WorkflowRunsJson = [Console]::In.ReadToEnd()
if ([string]::IsNullOrWhiteSpace($WorkflowRunsJson)) {
  throw "Workflow run JSON was not provided on standard input."
}
$workflowRuns = @(
  ($WorkflowRunsJson | ConvertFrom-Json) |
    ForEach-Object { $_ }
)
$matchingRuns = @(
  $workflowRuns |
    Where-Object {
      [string]$_.headBranch -ceq $ReleaseTag -and
      [string]$_.headSha -ceq $ExpectedCommit -and
      [string]$_.status -ceq "completed" -and
      [string]$_.conclusion -ceq "success"
    }
)
if ($matchingRuns.Count -ne 1) {
  throw "Expected exactly one successful tag workflow for $ReleaseTag at $ExpectedCommit; found $($matchingRuns.Count)."
}

$databaseId = [string]$matchingRuns[0].databaseId
if ($databaseId -notmatch '^[1-9][0-9]*$') {
  throw "The selected tag workflow has an invalid database ID."
}
Write-Output $databaseId
