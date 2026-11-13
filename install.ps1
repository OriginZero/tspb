# tspb installer for Windows.
#
#   irm https://raw.githubusercontent.com/OriginZero/tspb/main/install.ps1 | iex
#
# Environment variables (or parameters):
#   TSPB_INSTALL_DIR  where to put the binary (default: %LOCALAPPDATA%\Programs\tspb)
#   TSPB_VERSION      release to install, e.g. v0.2.0 (default: latest)
#   TSPB_MIRROR       GitHub mirror prefix, e.g. https://ghfast.top/
#
# Without TSPB_MIRROR the direct download is tried first, then public GitHub
# mirrors are used as fallbacks.
[CmdletBinding()]
param(
    [string]$Version = $env:TSPB_VERSION,
    [string]$Mirror = $env:TSPB_MIRROR,
    [string]$InstallDir
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

# LOCALAPPDATA is missing in some stripped-down sessions; fall back gracefully.
if (-not $InstallDir) {
    if ($env:TSPB_INSTALL_DIR) {
        $InstallDir = $env:TSPB_INSTALL_DIR
    } elseif ($env:LOCALAPPDATA) {
        $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\tspb'
    } elseif ($env:USERPROFILE) {
        $InstallDir = Join-Path $env:USERPROFILE 'AppData\Local\Programs\tspb'
    } else {
        $InstallDir = Join-Path (Get-Location).Path 'tspb'
    }
}

$repo = 'OriginZero/tspb'
$target = 'x86_64-pc-windows-msvc'
$mirrors = @(
    'https://ghfast.top/',
    'https://gh-proxy.com/',
    'https://ghproxy.net/',
    'https://hub.gitmirror.com/'
)

function Write-Step($Message) { Write-Host " > $Message" -ForegroundColor Green }
function Write-Warn($Message) { Write-Host " ! $Message" -ForegroundColor Yellow }

function Get-Remote {
    param([string]$Url, [string]$OutFile, [int]$TimeoutSec = 300)
    $candidates = if ($Mirror) {
        @("$Mirror$Url")
    } else {
        @($Url) + ($mirrors | ForEach-Object { "$_$Url" })
    }
    foreach ($candidate in $candidates) {
        try {
            Invoke-WebRequest -Uri $candidate -OutFile $OutFile -TimeoutSec $TimeoutSec -UseBasicParsing
            return $true
        } catch {
            Write-Verbose "failed: $candidate"
        }
    }
    return $false
}

Write-Host ''
Write-Host '  tspb installer'
Write-Host ''

if (-not $env:PROCESSOR_ARCHITECTURE) {
    $hostArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
} else {
    $hostArch = $env:PROCESSOR_ARCHITECTURE
}
if ($hostArch -match 'ARM64') {
    Write-Warn 'ARM64 host detected: installing the x64 build, which runs under emulation.'
}

if (-not $Version) {
    Write-Step 'resolving the latest release...'
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -UseBasicParsing
        $Version = $release.tag_name
    } catch {
        throw "cannot reach GitHub; retry with -Version vX.Y.Z or -Mirror https://your-mirror/"
    }
}
if ($Version -notlike 'v*') { $Version = "v$Version" }

$base = "https://github.com/$repo/releases/download/$Version"
$asset = "tspb-$Version-$target.zip"
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("tspb-install-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

try {
    Write-Step "installing $Version for $target"
    if (-not (Get-Remote "$base/$asset" (Join-Path $tmp $asset))) {
        throw "download failed: $base/$asset"
    }

    $sums = Join-Path $tmp 'SHA256SUMS.txt'
    if (Get-Remote "$base/SHA256SUMS.txt" $sums 30) {
        $expected = Get-Content $sums |
            Where-Object { $_.TrimEnd() -like "*$asset" } |
            ForEach-Object { $_.Split(' ')[0] } |
            Select-Object -First 1
        if ($expected) {
            $actual = (Get-FileHash -Path (Join-Path $tmp $asset) -Algorithm SHA256).Hash
            if ($actual -ne $expected.ToUpper()) { throw 'checksum mismatch, nothing was installed' }
            Write-Step 'checksum verified'
        }
    } else {
        Write-Warn 'no SHA256SUMS.txt for this release, skipping checksum verification'
    }

    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $exe = Join-Path $InstallDir 'tspb.exe'
    try {
        Copy-Item (Join-Path $tmp 'tspb.exe') $exe -Force
    } catch {
        throw "cannot overwrite $exe; close any running tspb window and retry"
    }
} finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $InstallDir) {
    $newPath = if ([string]::IsNullOrWhiteSpace($userPath)) { $InstallDir } else { $userPath.TrimEnd(';') + ';' + $InstallDir }
    try {
        [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        Write-Step "added $InstallDir to your user PATH (open a new terminal to pick it up)"
    } catch {
        Write-Warn "could not update the user PATH automatically; add this directory manually: $InstallDir"
    }
    $env:Path = "$env:Path;$InstallDir"
}

Write-Host ''
Write-Step "installed $(Join-Path $InstallDir 'tspb.exe')"
Write-Step "run 'tspb' inside a Spring Boot repository, 'tspb update' to upgrade."
Write-Host ''
