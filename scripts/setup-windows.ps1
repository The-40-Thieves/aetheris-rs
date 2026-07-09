#requires -Version 5.1
<#
.SYNOPSIS
    Scaffold the aetheris-rs project locally under E:\Projects on Windows.

.DESCRIPTION
    Creates the target root (default E:\Projects), clones (or updates) the
    aetheris-rs repository into it, verifies the toolchain prerequisites
    (Node.js, Rust/Cargo), installs npm dependencies, and prints next steps.

    Run this on your Windows workstation — the remote cloud session that
    produced this script runs on Linux and cannot reach your E: drive.

.PARAMETER Root
    Parent folder to create the project in. Default: E:\Projects

.PARAMETER RepoUrl
    Git remote to clone. Default: https://github.com/The-40-Thieves/aetheris-rs.git

.PARAMETER Branch
    Branch to check out. Default: master

.PARAMETER SkipInstall
    Skip `npm install`.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup-windows.ps1

.EXAMPLE
    .\setup-windows.ps1 -Root D:\dev -Branch master
#>
[CmdletBinding()]
param(
    [string]$Root = 'E:\Projects',
    [string]$RepoUrl = 'https://github.com/The-40-Thieves/aetheris-rs.git',
    [string]$Branch = 'master',
    [switch]$SkipInstall
)

$ErrorActionPreference = 'Stop'

function Write-Step($msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "  OK  $msg" -ForegroundColor Green }
function Write-Warn2($msg){ Write-Host "  !!  $msg" -ForegroundColor Yellow }

function Test-Cmd($name) {
    $null = Get-Command $name -ErrorAction SilentlyContinue
    return $?
}

Write-Step "Aetheris local project setup"
Write-Host "    Root:   $Root"
Write-Host "    Repo:   $RepoUrl"
Write-Host "    Branch: $Branch`n"

# 1. Ensure the parent root exists (e.g. E:\Projects).
$driveLetter = ($Root -split ':')[0]
if ($Root -match '^[A-Za-z]:' -and -not (Test-Path "${driveLetter}:\")) {
    throw "Drive ${driveLetter}: does not exist. Pass -Root to point at a real drive, e.g. -Root D:\Projects"
}
if (-not (Test-Path $Root)) {
    Write-Step "Creating $Root"
    New-Item -ItemType Directory -Path $Root -Force | Out-Null
    Write-Ok "created $Root"
} else {
    Write-Ok "$Root already exists"
}

# 2. Require git.
if (-not (Test-Cmd git)) {
    throw "git is not installed or not on PATH. Install Git for Windows: https://git-scm.com/download/win"
}

# 3. Clone or update.
$projectPath = Join-Path $Root 'aetheris-rs'
if (Test-Path (Join-Path $projectPath '.git')) {
    Write-Step "Repo already present — fetching latest ($Branch)"
    Push-Location $projectPath
    try {
        git fetch origin $Branch
        git checkout $Branch
        git pull --ff-only origin $Branch
        Write-Ok "updated $projectPath"
    } finally { Pop-Location }
} else {
    Write-Step "Cloning into $projectPath"
    git clone --branch $Branch $RepoUrl $projectPath
    Write-Ok "cloned $projectPath"
}

# 4. Verify prerequisites (warn, don't fail — the clone is still usable).
Write-Step "Checking prerequisites"
$missing = @()

if (Test-Cmd node) { Write-Ok "node  $(node --version)" }  else { Write-Warn2 "node not found";  $missing += 'Node.js (https://nodejs.org)' }
if (Test-Cmd npm)  { Write-Ok "npm   $(npm --version)" }   else { Write-Warn2 "npm not found";   $missing += 'npm (bundled with Node.js)' }
if (Test-Cmd cargo){ Write-Ok "cargo $(cargo --version)" } else { Write-Warn2 "cargo not found"; $missing += 'Rust & Cargo (https://rustup.rs)' }
if (Test-Cmd rustc){ Write-Ok "rustc $(rustc --version)" } else { Write-Warn2 "rustc not found"; $missing += 'Rust (https://rustup.rs)' }

# Optional data-source tools (not required to build, improve fidelity).
foreach ($opt in 'nvidia-smi','smartctl','docker') {
    if (Test-Cmd $opt) { Write-Ok "optional: $opt present" } else { Write-Warn2 "optional: $opt not found (that source will show 'unavailable')" }
}

# 5. npm install.
if (-not $SkipInstall -and (Test-Cmd npm) -and (Test-Path (Join-Path $projectPath 'package.json'))) {
    Write-Step "Installing npm dependencies"
    Push-Location $projectPath
    try { npm install; Write-Ok "npm install complete" }
    catch { Write-Warn2 "npm install failed: $($_.Exception.Message)" }
    finally { Pop-Location }
} elseif ($SkipInstall) {
    Write-Warn2 "skipping npm install (-SkipInstall)"
}

# 6. Summary.
Write-Host ""
Write-Step "Done"
Write-Host "    Project: $projectPath"
if ($missing.Count -gt 0) {
    Write-Warn2 "Install these before building:"
    $missing | ForEach-Object { Write-Host "      - $_" -ForegroundColor Yellow }
}
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "    cd `"$projectPath`""
Write-Host "    npm run dev      # launch the instrument-deck (dev)"
Write-Host "    npm run build    # release build"
Write-Host ""
Write-Host "Tip: run your terminal as Administrator for SMART (smartctl) and full"
Write-Host "egress attribution. See docs/repo-analysis.md for the architecture map."
