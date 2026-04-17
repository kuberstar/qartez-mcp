param(
    [switch]$Interactive,
    [switch]$SkipSetup,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

# Qartez MCP — native Windows installer
# Usage:
#   powershell -ExecutionPolicy Bypass -c "iwr https://raw.githubusercontent.com/kuberstar/qartez-mcp/main/install.ps1 -useb | iex"
#
# From a checked-out repo:
#   .\install.ps1

$Repo = 'kuberstar/qartez-mcp'
$Branch = 'main'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\qartez\bin'

function Write-Info([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message) { Write-Host "[+] $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) { Write-Host "[!] $Message" -ForegroundColor Yellow }

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$File,
        [Parameter(Mandatory = $true)][string[]]$Args
    )
    if ($DryRun) {
        Write-Info "[dry-run] $File $($Args -join ' ')"
        return
    }

    & $File @Args
    if ($LASTEXITCODE -ne 0) {
        throw "Command failed ($LASTEXITCODE): $File $($Args -join ' ')"
    }
}

function Add-ToUserPath {
    param([Parameter(Mandatory = $true)][string]$PathToAdd)

    $current = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if ([string]::IsNullOrWhiteSpace($current)) {
        $newPath = $PathToAdd
    }
    elseif ($current -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ -eq $PathToAdd }) {
        return
    }
    else {
        $newPath = "$PathToAdd;$current"
    }

    if ($DryRun) {
        Write-Info "[dry-run] Set user PATH to include: $PathToAdd"
        return
    }

    [Environment]::SetEnvironmentVariable('PATH', $newPath, 'User')
    if (-not (($env:PATH -split ';') -contains $PathToAdd)) {
        $env:PATH = "$PathToAdd;$env:PATH"
    }
}

function Resolve-SourceDirectory {
    # Repo mode: script sits next to Cargo.toml
    $repoDir = if ($PSCommandPath) {
        Split-Path -Parent $PSCommandPath
    }
    else {
        (Get-Location).Path
    }
    $cargoToml = Join-Path $repoDir 'Cargo.toml'
    if (Test-Path -Path $cargoToml -PathType Leaf) {
        return $repoDir
    }

    # Download source archive from GitHub and extract
    Write-Info "Source not found locally - downloading from github.com/$Repo..."

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("qartez-install-" + [guid]::NewGuid().ToString('N'))
    $archive = Join-Path $tmp 'qartez.zip'
    $extractDir = Join-Path $tmp 'src'
    if (-not $DryRun) {
        New-Item -ItemType Directory -Path $extractDir -Force | Out-Null
        $url = "https://codeload.github.com/$Repo/zip/refs/heads/$Branch"
        Invoke-WebRequest -Uri $url -OutFile $archive -UseBasicParsing
        Expand-Archive -Path $archive -DestinationPath $extractDir -Force
    }
    else {
        Write-Info "[dry-run] Download and extract source archive from github"
        return $repoDir
    }

    $sourceDir = Get-ChildItem -Path $extractDir -Directory | Select-Object -First 1
    if (-not $sourceDir) {
        throw 'Unexpected source archive layout (no extracted root directory found).'
    }
    $sourceCargoToml = Join-Path $sourceDir.FullName 'Cargo.toml'
    if (-not (Test-Path -Path $sourceCargoToml -PathType Leaf)) {
        throw "Unexpected source archive layout: $sourceCargoToml not found"
    }

    return $sourceDir.FullName
}

function Ensure-Cargo {
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if ($cargo) {
        return $cargo.Source
    }

    $cargoHomeBin = Join-Path $env:USERPROFILE '.cargo\bin'
    $cargoExe = Join-Path $cargoHomeBin 'cargo.exe'
    if (Test-Path -Path $cargoExe -PathType Leaf) {
        if (-not (($env:PATH -split ';') -contains $cargoHomeBin)) {
            $env:PATH = "$cargoHomeBin;$env:PATH"
        }
        return $cargoExe
    }

    Write-Info 'Rust not found. Installing via rustup...'
    $rustupInit = Join-Path ([System.IO.Path]::GetTempPath()) 'rustup-init.exe'
    if (-not $DryRun) {
        Invoke-WebRequest -Uri 'https://win.rustup.rs/x86_64' -OutFile $rustupInit -UseBasicParsing
        Invoke-Checked -File $rustupInit -Args @('-y')
        Remove-Item -Path $rustupInit -Force -ErrorAction SilentlyContinue
    }
    else {
        Write-Info '[dry-run] Download and run rustup-init.exe -y'
    }

    if (-not (($env:PATH -split ';') -contains $cargoHomeBin)) {
        $env:PATH = "$cargoHomeBin;$env:PATH"
    }

    if ($DryRun) {
        return $cargoExe
    }

    if (-not (Test-Path -Path $cargoExe -PathType Leaf)) {
        throw "cargo not found at $cargoExe after rustup install"
    }
    Write-Ok 'Rust installed.'
    return $cargoExe
}

Write-Info 'Installing qartez-mcp (Windows native)...'

$sourceDir = Resolve-SourceDirectory
$cargoPath = Ensure-Cargo

Write-Info 'Building release binaries (first build may take a few minutes)...'
$oldLocation = Get-Location
try {
    Set-Location $sourceDir
    Invoke-Checked -File $cargoPath -Args @('build', '--release')
}
finally {
    Set-Location $oldLocation
}

$targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $sourceDir 'target' }
$releaseDir = Join-Path $targetDir 'release'

if (-not $DryRun) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
else {
    Write-Info "[dry-run] Ensure install directory exists: $InstallDir"
}

foreach ($bin in @('qartez-mcp', 'qartez-guard', 'qartez-setup')) {
    $src = Join-Path $releaseDir ($bin + '.exe')
    if (-not $DryRun -and -not (Test-Path -Path $src -PathType Leaf)) {
        throw "Binary not found: $src"
    }

    $tmp = Join-Path $InstallDir ($bin + '.new.exe')
    $dst = Join-Path $InstallDir ($bin + '.exe')
    if (-not $DryRun) {
        Copy-Item -Path $src -Destination $tmp -Force
        Move-Item -Path $tmp -Destination $dst -Force
        $sizeMB = (Get-Item $src).Length / 1MB
        Write-Ok ("Installed: {0} ({1:N1} MB)" -f $dst, $sizeMB)
    }
    else {
        Write-Info "[dry-run] Install $src -> $dst (atomic via *.new.exe)"
    }
}

Add-ToUserPath -PathToAdd $InstallDir
Write-Ok "User PATH includes: $InstallDir"

$setupExe = Join-Path $InstallDir 'qartez-setup.exe'
if ($Interactive) {
    Write-Info 'Launching interactive IDE setup...'
    Invoke-Checked -File $setupExe -Args @()
}
elseif ($SkipSetup) {
    Write-Info 'Skipping IDE setup (--SkipSetup).'
}
else {
    Write-Info 'Configuring all detected IDEs...'
    Invoke-Checked -File $setupExe -Args @('--yes')
}

Write-Ok 'Install complete. Restart your terminal and IDEs to pick up PATH/MCP changes.'
