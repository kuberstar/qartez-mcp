$ErrorActionPreference = 'Stop'

function Step($name) { Write-Host "==> $name" -ForegroundColor Cyan }

$repo = Split-Path -Parent $PSScriptRoot
$install = Join-Path $repo 'install.ps1'

Step 'Validate install.ps1 presence'
if (-not (Test-Path -Path $install -PathType Leaf)) {
    throw "install.ps1 missing at $install"
}

Step 'Dry-run installer'
& powershell -ExecutionPolicy Bypass -File $install -DryRun -SkipSetup
if ($LASTEXITCODE -ne 0) {
    throw "install.ps1 dry-run failed with code $LASTEXITCODE"
}

Step 'PowerShell installer checks passed'
