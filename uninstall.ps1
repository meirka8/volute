$ErrorActionPreference = "Continue"

Write-Host "Uninstalling CVC..."

$PathsToRemove = @(
    "$env:USERPROFILE\.cvc",
    "$env:LOCALAPPDATA\volute\cvc",
    "$env:APPDATA\volute\cvc"
)

foreach ($p in $PathsToRemove) {
    if (Test-Path $p) {
        Write-Host "Removing directory: $p"
        Remove-Item -Path $p -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "`nCVC has been successfully uninstalled from your user profile."
Write-Host "Note: Repository CVC state is intentionally left in place. In a repository, git rev-parse --git-common-dir identifies its common Git directory; CVC data is in its cvc directory and is shared by linked worktrees."
Write-Host "CVC refs (refs/cvc), related Git objects/reflogs, and hooks may also remain. Hooks use Git's effective hooks path (the common directory's hooks directory by default, or core.hooksPath). Cleanup, if wanted, is manual."
