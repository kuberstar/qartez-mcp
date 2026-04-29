param(
    [switch]$Interactive,
    [switch]$SkipSetup,
    [switch]$FromSource,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'

# Qartez MCP - native Windows installer
# Usage:
#   powershell -ExecutionPolicy Bypass -c "iwr https://raw.githubusercontent.com/kuberstar/qartez-mcp/main/install.ps1 -useb | iex"
#
# From a checked-out repo:
#   .\install.ps1
#
# By default this installer downloads a pre-built release zip for
# x86_64-pc-windows-msvc. Use -FromSource to force a cargo build.

$Repo = 'kuberstar/qartez-mcp'
$Branch = 'main'
$InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\qartez\bin'
$BinaryNames = @('qartez', 'qartez-guard', 'qartez-setup')
$Target = 'x86_64-pc-windows-msvc'

function Write-Info([string]$Message) { Write-Host "==> $Message" -ForegroundColor Cyan }
function Write-Ok([string]$Message) { Write-Host "[+] $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) { Write-Host "[!] $Message" -ForegroundColor Yellow }
function Write-Err([string]$Message) { Write-Host "[!] $Message" -ForegroundColor Red }

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

function Install-BinaryFile {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    if ($DryRun) {
        Write-Info "[dry-run] Install $Source -> $Destination (atomic via *.new)"
        return
    }
    $tmp = "$Destination.new"
    Copy-Item -Path $Source -Destination $tmp -Force
    Move-Item -Path $tmp -Destination $Destination -Force
}

function Get-LatestReleaseTag {
    $uri = "https://api.github.com/repos/$Repo/releases/latest"
    try {
        $resp = Invoke-WebRequest -Uri $uri -UseBasicParsing -Headers @{ 'User-Agent' = 'qartez-installer' }
        $json = $resp.Content | ConvertFrom-Json
        return $json.tag_name
    }
    catch {
        return $null
    }
}

function Install-FromPrebuilt {
    if ($DryRun) {
        Write-Info "[dry-run] Resolve latest release and download qartez-<ver>-$Target.zip"
        Write-Info "[dry-run] Verify SHA256SUMS via Get-FileHash"
        Write-Info "[dry-run] Extract and install binaries to $InstallDir"
        return $true
    }

    Write-Info "Resolving latest release tag for $Repo..."
    $tag = Get-LatestReleaseTag
    if (-not $tag) {
        Write-Warn 'Could not resolve latest release tag (network error or API rate limit).'
        return $false
    }
    $version = $tag.TrimStart('v')
    Write-Ok "Latest release: $tag"

    $archiveName = "qartez-$version-$Target.zip"
    $baseUrl = "https://github.com/$Repo/releases/download/$tag"
    $archiveUrl = "$baseUrl/$archiveName"
    $sumsUrl = "$baseUrl/SHA256SUMS"

    $tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("qartez-install-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp -Force | Out-Null
    try {
        $archivePath = Join-Path $tmp $archiveName
        $sumsPath = Join-Path $tmp 'SHA256SUMS'
        $extractDir = Join-Path $tmp 'extract'

        Write-Info "Downloading $archiveName..."
        try {
            Invoke-WebRequest -Uri $archiveUrl -OutFile $archivePath -UseBasicParsing
        }
        catch {
            Write-Warn "No pre-built binary for $Target at $tag - falling back to source build."
            return $false
        }

        Write-Info 'Verifying checksum...'
        try {
            Invoke-WebRequest -Uri $sumsUrl -OutFile $sumsPath -UseBasicParsing
        }
        catch {
            Write-Warn "SHA256SUMS not available at $tag - falling back to source build."
            return $false
        }

        $expected = $null
        foreach ($line in Get-Content -LiteralPath $sumsPath) {
            $parts = $line -split '\s+', 2
            if ($parts.Count -eq 2 -and $parts[1].Trim() -eq $archiveName) {
                $expected = $parts[0].Trim().ToLowerInvariant()
                break
            }
        }
        if (-not $expected) {
            Write-Warn "Checksum for $archiveName missing from SHA256SUMS - falling back to source build."
            return $false
        }

        $actual = (Get-FileHash -Algorithm SHA256 -Path $archivePath).Hash.ToLowerInvariant()
        if ($expected -ne $actual) {
            Write-Err "Checksum mismatch for ${archiveName}:"
            Write-Err "  expected: $expected"
            Write-Err "  actual:   $actual"
            Write-Err 'Refusing to install. This can indicate a corrupted download'
            Write-Err 'or a tampered release asset. Re-run to retry, or pass'
            Write-Err '-FromSource to build from source instead.'
            throw 'checksum mismatch'
        }
        Write-Ok 'Checksum verified'

        Write-Info 'Extracting archive...'
        Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

        $stageRoot = Join-Path $extractDir "qartez-$version-$Target"
        if (-not (Test-Path -Path $stageRoot -PathType Container)) {
            Write-Err "Archive layout unexpected: $stageRoot not found"
            return $false
        }

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        foreach ($bin in $BinaryNames) {
            $src = Join-Path $stageRoot ($bin + '.exe')
            $dst = Join-Path $InstallDir ($bin + '.exe')
            if (-not (Test-Path -Path $src -PathType Leaf)) {
                Write-Err "Missing binary in archive: $src"
                return $false
            }
            Install-BinaryFile -Source $src -Destination $dst
            $sizeMB = (Get-Item $src).Length / 1MB
            Write-Ok ("Installed: {0} ({1:N1} MB)" -f $dst, $sizeMB)
        }
        return $true
    }
    finally {
        Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Resolve-SourceDirectory {
    # Repo mode: script sits next to Cargo.toml. When invoked via
    # `iwr ... | iex`, $PSCommandPath is $null, so a fallback to (Get-Location)
    # would silently anchor $repoDir to the user's CWD - and any random rust
    # project they happened to be inside would then be mistaken for the qartez
    # source tree. Skip the fallback and download the source archive instead.
    $repoDir = if ($PSCommandPath) {
        Split-Path -Parent $PSCommandPath
    }
    else {
        $null
    }
    if ($repoDir) {
        $cargoToml = Join-Path $repoDir 'Cargo.toml'
        if ((Test-Path -Path $cargoToml -PathType Leaf) -and `
            (Select-String -Path $cargoToml -Pattern '^name *= *"qartez-mcp"' -Quiet)) {
            return $repoDir
        }
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

function Install-FromSource {
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

    foreach ($bin in $BinaryNames) {
        $src = Join-Path $releaseDir ($bin + '.exe')
        if (-not $DryRun -and -not (Test-Path -Path $src -PathType Leaf)) {
            throw "Binary not found: $src"
        }

        $dst = Join-Path $InstallDir ($bin + '.exe')
        Install-BinaryFile -Source $src -Destination $dst
        if (-not $DryRun) {
            $sizeMB = (Get-Item $src).Length / 1MB
            Write-Ok ("Installed: {0} ({1:N1} MB)" -f $dst, $sizeMB)
        }
    }
}

# Running from a checked-out repo always builds from source - that matches the
# dev workflow and avoids shadowing local changes. For remote installs, try
# pre-built first and fall through to source on any failure.
#
# When invoked via `iwr ... | iex`, $PSCommandPath is $null. Falling back to
# (Get-Location) would silently anchor $repoCandidate to the user's CWD, and
# a Cargo.toml in any unrelated rust project would flip $LocalRepo to true -
# exactly the bug reported in qartez-mcp#31 for the POSIX installer. Only
# treat the script directory as the local source tree when (a) we actually
# know the script's path, and (b) its Cargo.toml belongs to qartez-mcp.
$LocalRepo = $false
if ($PSCommandPath) {
    $repoCandidate = Split-Path -Parent $PSCommandPath
    $cargoToml = Join-Path $repoCandidate 'Cargo.toml'
    if ((Test-Path -Path $cargoToml -PathType Leaf) -and `
        (Select-String -Path $cargoToml -Pattern '^name *= *"qartez-mcp"' -Quiet)) {
        $LocalRepo = $true
    }
}

Write-Info 'Installing qartez (Windows native)...'

$installed = $false
if (-not $FromSource -and -not $LocalRepo) {
    $installed = Install-FromPrebuilt
}
elseif ($FromSource) {
    Write-Info 'Forced source build (-FromSource).'
}

if (-not $installed) {
    Install-FromSource
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
