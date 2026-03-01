$ErrorActionPreference = "Continue"

Write-Host "Uninstalling CVC..."

$PathsToRemove = @(
    "$env:USERPROFILE\.cvc",
    "$env:LOCALAPPDATA\helixthought\cvc",
    "$env:APPDATA\helixthought\cvc"
)

foreach ($p in $PathsToRemove) {
    if (Test-Path $p) {
        Write-Host "Removing directory: $p"
        Remove-Item -Path $p -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "`nCVC has been successfully uninstalled from your user profile."
Write-Host "Note: If you have initialized CVC in any local Git repositories, the .git\cvc databases and hooks still exist in those specific directories. You can safely delete them manually if desired."
