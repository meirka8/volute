$ErrorActionPreference = "Stop"

$Repo = "meirka8/cvc"
$InstallDir = "$env:USERPROFILE\.cvc\bin"

Write-Host "Installing CVC..."

# Detect Arch
$Arch = $env:PROCESSOR_ARCHITECTURE
if ($Arch -eq "AMD64") {
    $ArchTag = "x86_64"
    $OsTag = "pc-windows-msvc"
} else {
    Write-Error "Unsupported Architecture: $Arch"
    exit 1
}

$AssetName = "cvc-$ArchTag-$OsTag.zip"

Write-Host "Detected Platform: Windows $Arch"
Write-Host "Target Asset: $AssetName"

# Create install directory
if (!(Test-Path -Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

# Download logic (placeholder until releases exist)
$DownloadUrl = "https://github.com/$Repo/releases/latest/download/$AssetName"

Write-Host "Downloading from $DownloadUrl..."
# Invoke-WebRequest -Uri $DownloadUrl -OutFile "$env:TEMP\$AssetName"

# Extract (release archive contains both cvc.exe and cvc-mcp.exe)
# Expand-Archive -Path "$env:TEMP\$AssetName" -DestinationPath $InstallDir -Force

Write-Host "NOTE: release download is commented out until releases exist."
Write-Host "Simulating installation..."
New-Item -ItemType File -Force -Path "$InstallDir\cvc.exe" | Out-Null
New-Item -ItemType File -Force -Path "$InstallDir\cvc-mcp.exe" | Out-Null

Write-Host ""
Write-Host "Success! CVC installed to $InstallDir"
Write-Host "  cvc.exe       - CLI interface"
Write-Host "  cvc-mcp.exe   - MCP server for coding agents"
Write-Host ""
Write-Host "Please add $InstallDir to your User PATH."
Write-Host ""
Write-Host "To use CVC with a coding agent (Claude, Cursor, Windsurf, etc.),"
Write-Host "add the following to your MCP client config:"
Write-Host ""
Write-Host "  {`"cvc`": {`"command`": `"$InstallDir\cvc-mcp.exe`", `"args`": []}}"
Write-Host ""
