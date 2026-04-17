$ErrorActionPreference = 'Stop'

$script:Pass = 0
$script:Fail = 0

function Step($name) { Write-Host "==> $name" -ForegroundColor Cyan }
function PassResult($msg) { $script:Pass++; Write-Host "  PASS $msg" -ForegroundColor Green }
function FailResult($msg) { $script:Fail++; Write-Host "  FAIL $msg" -ForegroundColor Red }

function Assert-Contains {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Pattern,
        [Parameter(Mandatory = $true)][string]$Label
    )
    $text = Get-Content -LiteralPath $Path -Raw
    if ($text -match [regex]::Escape($Pattern)) {
        PassResult $Label
    }
    else {
        FailResult "$Label (missing pattern: $Pattern)"
    }
}

$repo = Split-Path -Parent $PSScriptRoot
$install = Join-Path $repo 'install.ps1'

Step 'Validate install.ps1 presence'
if (-not (Test-Path -Path $install -PathType Leaf)) {
    throw "install.ps1 missing at $install"
}

Step 'Static checks for pre-built bootstrap'
Assert-Contains -Path $install -Pattern 'x86_64-pc-windows-msvc' -Label 'target triple for Windows'
Assert-Contains -Path $install -Pattern 'api.github.com/repos' -Label 'latest-release API lookup'
Assert-Contains -Path $install -Pattern 'Get-FileHash' -Label 'Get-FileHash checksum verification'
Assert-Contains -Path $install -Pattern 'SHA256SUMS' -Label 'SHA256SUMS asset download'
Assert-Contains -Path $install -Pattern '-FromSource' -Label 'source-build escape hatch'
Assert-Contains -Path $install -Pattern 'Install-FromPrebuilt' -Label 'Install-FromPrebuilt function'
Assert-Contains -Path $install -Pattern 'falling back to source build' -Label 'source-build fallback message'
Assert-Contains -Path $install -Pattern 'Refusing to install' -Label 'checksum mismatch hard fail'
Assert-Contains -Path $install -Pattern "throw 'checksum mismatch'" -Label 'checksum mismatch throws'
Assert-Contains -Path $install -Pattern "qartez', 'qartez-guard', 'qartez-setup'" -Label 'binary list matches Cargo.toml bins'

$onWindows = $IsWindows -or ($PSVersionTable.PSVersion.Major -lt 6)
if (-not $onWindows) {
    Write-Host "==> Skipping dry-run (install.ps1 is Windows-only; static checks suffice on $($PSVersionTable.Platform))"
}
else {
    Step 'Dry-run installer'
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $install -DryRun -SkipSetup
    if ($LASTEXITCODE -ne 0) {
        throw "install.ps1 dry-run failed with code $LASTEXITCODE"
    }

    Step 'Dry-run installer (-FromSource)'
    & pwsh -NoProfile -ExecutionPolicy Bypass -File $install -DryRun -SkipSetup -FromSource
    if ($LASTEXITCODE -ne 0) {
        throw "install.ps1 -FromSource dry-run failed with code $LASTEXITCODE"
    }
}

Step 'PowerShell installer checks summary'
Write-Host ("  {0} passed, {1} failed" -f $script:Pass, $script:Fail)
if ($script:Fail -gt 0) {
    throw "static install.ps1 checks failed"
}
