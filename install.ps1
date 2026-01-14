$ErrorActionPreference = "Stop"

$Repo = "helixthought/cvc2"
$InstallDir = "$env:USERPROFILE\.cvc\bin"
$BinaryName = "cvc.exe"

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

# Download logic matching install.sh placeholder
$DownloadUrl = "https://github.com/$Repo/releases/latest/download/$AssetName"

Write-Host "Downloading from $DownloadUrl..."
# Invoke-WebRequest -Uri $DownloadUrl -OutFile "$env:TEMP\$AssetName"

# Extract
# Expand-Archive -Path "$env:TEMP\$AssetName" -DestinationPath $InstallDir -Force

Write-Host "NOTE: specific release URL download is commented out until releases exist."
Write-Host "Simulating installation..."
New-Item -ItemType File -Force -Path "$InstallDir\$BinaryName" | Out-Null

Write-Host "Success! CVC installed to $InstallDir"
Write-Host "Please add $InstallDir to your User PATH."
