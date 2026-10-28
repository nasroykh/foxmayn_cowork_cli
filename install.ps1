#Requires -Version 5.1
[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$Repo   = "nasroykh/foxmayn_cowork_cli"
$Binary = "foxmayn-cowork.exe"

# --- detect arch ---
$Arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    # No native ARM64 build yet; Windows on ARM runs the x64 binary under emulation.
    "ARM64" { "x86_64" }
    default {
        Write-Error "Unsupported architecture: $env:PROCESSOR_ARCHITECTURE"
        exit 1
    }
}

$Target = "${Arch}-pc-windows-msvc"

# Windows PowerShell 5.1 defaults to older TLS versions that GitHub rejects.
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

# --- resolve latest release tag ---
$Release    = Invoke-RestMethod "https://api.github.com/repos/$Repo/releases/latest"
$Version    = $Release.tag_name   # e.g. "v0.2.0"

Write-Host "Installing foxmayn-cowork $Version (windows/$Arch)..."

$Archive      = "foxmayn-cowork_${Version}_${Target}.zip"
$DownloadUrl  = "https://github.com/$Repo/releases/download/$Version/$Archive"
$ChecksumUrl  = "https://github.com/$Repo/releases/download/$Version/foxmayn-cowork_${Version}_${Target}.sha256"

$TmpDir = Join-Path $env:TEMP "foxmayn_cowork_install_$([System.IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory -Path $TmpDir | Out-Null

try {
    $ArchivePath  = Join-Path $TmpDir $Archive
    $ChecksumPath = Join-Path $TmpDir "${Archive}.sha256"

    # --- download ---
    Invoke-WebRequest -Uri $DownloadUrl  -OutFile $ArchivePath  -UseBasicParsing
    Invoke-WebRequest -Uri $ChecksumUrl  -OutFile $ChecksumPath -UseBasicParsing

    # --- verify checksum ---
    $ChecksumText = (Get-Content $ChecksumPath -Raw)
    if ([string]::IsNullOrWhiteSpace($ChecksumText)) {
        Write-Error "Checksum file is empty; refusing to install."
        exit 1
    }
    $Expected = ($ChecksumText.Trim() -split '\s+' | Select-Object -First 1).ToLower()
    if ($Expected -notmatch '^[0-9a-f]{64}$') {
        Write-Error "Checksum file is malformed (expected a SHA256 hex digest); refusing to install."
        exit 1
    }
    $Actual = (Get-FileHash -Algorithm SHA256 -Path $ArchivePath).Hash.ToLower()

    if ($Actual -ne $Expected) {
        Write-Error "Checksum mismatch!`n  Expected: $Expected`n  Got:      $Actual"
        exit 1
    }

    # --- extract ---
    Expand-Archive -Path $ArchivePath -DestinationPath $TmpDir -Force

    # --- choose install dir ---
    $InstallDir = "$env:LOCALAPPDATA\Programs\foxmayn-cowork"
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null

    Move-Item -Path (Join-Path $TmpDir $Binary) -Destination (Join-Path $InstallDir $Binary) -Force

    # --- add to user PATH if missing ---
    $UserPath = [System.Environment]::GetEnvironmentVariable("PATH", "User")
    if ($null -eq $UserPath) { $UserPath = "" }
    $PathEntries = @($UserPath -split ';' | Where-Object { $_ -ne "" })
    $Normalized  = $InstallDir.TrimEnd('\')
    if (-not ($PathEntries | Where-Object { $_.TrimEnd('\') -ieq $Normalized })) {
        $NewPath = (@($PathEntries) + $InstallDir) -join ';'
        [System.Environment]::SetEnvironmentVariable("PATH", $NewPath, "User")
        Write-Host ""
        Write-Host "Added $InstallDir to your PATH."
        Write-Host "Restart your terminal for the change to take effect."
    }

    Write-Host ""
    Write-Host "Installed to $InstallDir\$Binary"
    Write-Host "Run 'foxmayn-cowork --help' to get started."

} finally {
    Remove-Item -Recurse -Force $TmpDir -ErrorAction SilentlyContinue
}
