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

# Download logic via the secure proxy
$DownloadUrl = "https://cvc.dev/api/download/$AssetName"

Write-Host "Downloading from $DownloadUrl..."
Invoke-WebRequest -Uri $DownloadUrl -OutFile "$env:TEMP\$AssetName"

# Extract (release archive contains cvc.exe, cvc-mcp.exe, cvc-lsp.exe)
Expand-Archive -Path "$env:TEMP\$AssetName" -DestinationPath $InstallDir -Force

Write-Host ""
Write-Host "Success! CVC installed to $InstallDir"
Write-Host "  cvc.exe       - CLI interface"
Write-Host "  cvc-mcp.exe   - MCP server for coding agents"
Write-Host "  cvc-lsp.exe   - Language server for the VSCode extension"
Write-Host ""
$addPath = Read-Host "Would you like to add CVC to your User PATH? [Y/n]"
if ($addPath -eq 'n' -or $addPath -eq 'N') {
    Write-Host "Skipping PATH configuration."
    Write-Host "Please manually add $InstallDir to your User PATH."
} else {
    $userPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($userPath -like "*$InstallDir*") {
        Write-Host "CVC is already in your PATH."
    } else {
        if ($userPath -and -not $userPath.EndsWith(";")) {
            $newPath = $userPath + ";" + $InstallDir
        } else {
            $newPath = $userPath + $InstallDir
        }
        [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
        Write-Host "✔ Added $InstallDir to your User PATH."
        Write-Host "👉 Please restart your terminal for the new PATH to take effect."
    }
}
Write-Host ""
Write-Host "To use CVC with a coding agent (Claude, Cursor, Windsurf, etc.),"
Write-Host "add the following to your MCP client config:"
Write-Host ""
Write-Host "  {`"cvc`": {`"command`": `"$InstallDir\cvc-mcp.exe`", `"args`": []}}"
Write-Host ""
