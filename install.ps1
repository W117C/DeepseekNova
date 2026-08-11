# DeepseekNova CLI — one-line installer (Windows PowerShell 5.1+ / pwsh 7+)
#
# Usage:
#   irm https://raw.githubusercontent.com/W117C/DeepseekNova/main/install.ps1 | iex
#   powershell -ExecutionPolicy Bypass -File install.ps1
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Version 0.5.0
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Version 0.5.0 -InstallDir C:\tools
#
# Downloads the release binary for x86_64 Windows, verifies its SHA-256 against
# the release's checksums.txt, then installs it to $HOME\.deepseeknova\bin.
#
# Naming contract (must match .github/workflows/release.yml):
#   asset    = deepseeknova-cli-x86_64-pc-windows-msvc.zip
#   checksums.txt lines = "<sha256hex>  <path>" (sha256sum output, double space)
#   binary inside the archive: deepseeknova-cli.exe

param(
    [string]$Version,
    [string]$InstallDir
)

$ErrorActionPreference = 'Stop'

# TLS 1.2+ required by GitHub (PS 5.1 defaults to TLS 1.0; no-op on pwsh 7+).
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Older .NET — proceed anyway; Invoke-WebRequest will surface any failure.
}

$Repo       = 'W117C/DeepseekNova'
$ReleaseBase = "https://github.com/$Repo/releases/download"
$ApiLatest  = "https://api.github.com/repos/$Repo/releases/latest"

if ([string]::IsNullOrEmpty($InstallDir)) {
    $InstallDir = Join-Path $HOME '.deepseeknova\bin'
}

# ---------------------------------------------------------------------------
# Resolve version -> release tag
# ---------------------------------------------------------------------------
if ([string]::IsNullOrEmpty($Version)) {
    Write-Host "Resolving latest release from GitHub API: $ApiLatest"
    $latest = Invoke-RestMethod -Uri $ApiLatest -Headers @{ 'User-Agent' = 'deepseeknova-install' }
    $tag = $latest.tag_name
    if ([string]::IsNullOrEmpty($tag)) {
        throw "Could not parse tag_name from GitHub API response"
    }
    Write-Host "Latest release tag: $tag"
} else {
    $tag = if ($Version -like 'v*') { $Version } else { "v$Version" }
    if ($tag -notmatch '^v\d') {
        throw "Invalid version '$Version' (expected something like 0.4.0)"
    }
}

# ---------------------------------------------------------------------------
# Platform detection -> target triple (Windows x86_64 only)
# ---------------------------------------------------------------------------
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -ne 'AMD64') {
    throw "Unsupported CPU architecture '$arch'. DeepseekNova Windows installer supports only x86_64. Supported platforms: macOS (aarch64/x86_64), Linux (aarch64/x86_64), Windows (x86_64)."
}
$target = 'x86_64-pc-windows-msvc'
$asset  = "deepseeknova-cli-$target.zip"
$url    = "$ReleaseBase/$tag/$asset"
Write-Host "Platform: windows/$arch -> target: $target"
Write-Host "Downloading $url"

# ---------------------------------------------------------------------------
# Download asset + checksums.txt into a temp dir (always cleaned up)
# ---------------------------------------------------------------------------
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("dsn-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    $assetPath    = Join-Path $tmp $asset
    $checksumsPath = Join-Path $tmp 'checksums.txt'
    Invoke-WebRequest -Uri $url -OutFile $assetPath -UseBasicParsing
    Invoke-WebRequest -Uri "$ReleaseBase/$tag/checksums.txt" -OutFile $checksumsPath -UseBasicParsing

    # -----------------------------------------------------------------------
    # SHA-256 verification — match by asset basename only (checksums.txt paths
    # have a subdirectory prefix, e.g. "./<artifact-dir>/<asset>").
    # -----------------------------------------------------------------------
    $expected = $null
    foreach ($line in Get-Content -Path $checksumsPath) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $parts = $line -split '\s+', 2
        if ($parts.Count -lt 2) { continue }
        $base = $parts[1].Trim() -replace '.*[/\\]', ''
        if ($base -eq $asset) {
            $expected = $parts[0].Trim().ToLowerInvariant()
            break
        }
    }
    if ([string]::IsNullOrEmpty($expected)) {
        throw "No SHA-256 checksum entry for $asset in checksums.txt (release $tag may not contain it)"
    }
    if ($expected -notmatch '^[0-9a-f]{64}$') {
        throw "Malformed SHA-256 checksum for $asset in checksums.txt"
    }

    $actual = (Get-FileHash -Path $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Integrity check FAILED for $asset (expected $expected, actual $actual) — refusing to install. Downloaded file removed."
    }
    Write-Host "Checksum OK: $asset"

    # -----------------------------------------------------------------------
    # Install
    # -----------------------------------------------------------------------
    Expand-Archive -Path $assetPath -DestinationPath $tmp -Force
    $exePath = Join-Path $tmp 'deepseeknova-cli.exe'
    if (-not (Test-Path -Path $exePath)) {
        throw "Unexpected archive layout: deepseeknova-cli.exe not found in $asset"
    }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    Copy-Item -Path $exePath -Destination (Join-Path $InstallDir 'deepseeknova-cli.exe') -Force
    Write-Host "Installed DeepseekNova CLI ($tag) to $InstallDir"
    Write-Host "Run it with: deepseeknova-cli --version"

    # -----------------------------------------------------------------------
    # PATH hint
    # -----------------------------------------------------------------------
    if ($env:PATH -notlike "*$InstallDir*") {
        Write-Host ""
        Write-Host "NOTE: $InstallDir is not on your PATH."
        Write-Host "  Add it for your user (persists across sessions), then open a new terminal:"
        Write-Host "    [Environment]::SetEnvironmentVariable('Path', [Environment]::GetEnvironmentVariable('Path','User') + ';' + '$InstallDir', 'User')"
        Write-Host "  Or run it directly now: $InstallDir\deepseeknova-cli.exe --version"
    } else {
        Write-Host "DeepseekNova CLI is ready. Run 'deepseeknova-cli --version' to confirm."
    }

    # -----------------------------------------------------------------------
    # Next-step hint
    # -----------------------------------------------------------------------
    Write-Host ""
    Write-Host "Next:"
    Write-Host "  1. deepseeknova-cli setup           # interactive provider/model/key config"
    Write-Host "  2. `$env:DEEPSEEK_API_KEY = 'sk-...'  # set your API key (or as prompted)"
    Write-Host "  3. deepseeknova-cli chat --tui      # launch the interactive terminal UI"
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
