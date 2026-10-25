$ErrorActionPreference = "Stop"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:CVC_RELEASE_REPOSITORY) { $env:CVC_RELEASE_REPOSITORY } else { "meirka8/volute" }
$ReleaseBaseUrl = if ($env:CVC_RELEASE_BASE_URL) { $env:CVC_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com" }
$InstallDir = if ($env:CVC_INSTALL_DIR) { $env:CVC_INSTALL_DIR } else { Join-Path $env:USERPROFILE ".cvc\bin" }
$ReleaseVersion = if ($env:CVC_RELEASE_VERSION) { $env:CVC_RELEASE_VERSION } else { "latest" }
if ($Repo -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' -or $Repo.Contains('..')) { throw "CVC_RELEASE_REPOSITORY must be a safe owner/repository name." }
$BaseUri = $null
if (-not [Uri]::TryCreate($ReleaseBaseUrl, [UriKind]::Absolute, [ref]$BaseUri) -or $BaseUri.Scheme -ne 'https') { throw "CVC_RELEASE_BASE_URL must use HTTPS." }
if (-not [string]::IsNullOrEmpty($BaseUri.UserInfo) -or -not [string]::IsNullOrEmpty($BaseUri.Query) -or -not [string]::IsNullOrEmpty($BaseUri.Fragment)) { throw "CVC_RELEASE_BASE_URL must not contain credentials, a query, or a fragment." }
if (-not [IO.Path]::IsPathRooted($InstallDir) -or $InstallDir -match '[\x00-\x1f;*?]') { throw "CVC_INSTALL_DIR must be a safe absolute filesystem path." }
$InstallDir = [IO.Path]::GetFullPath($InstallDir)
# The release archive also carries LICENSE and THIRD-PARTY-NOTICES.md. They are
# installed into a share directory beside the install directory:
# %USERPROFILE%\.cvc\bin gives %USERPROFILE%\.cvc\share\cvc, and a custom
# <prefix>\bin gives <prefix>\share\cvc.
$SharePrefix = [IO.Path]::GetFullPath((Join-Path $InstallDir '..'))
$ShareDir = Join-Path (Join-Path $SharePrefix 'share') 'cvc'
if ($ReleaseVersion -ne "latest" -and $ReleaseVersion -notmatch '^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$') {
    throw "CVC_RELEASE_VERSION must be exactly v<semver> (without build metadata), or unset."
}

$MaxArchiveBytes = 256MB
$MaxChecksumBytes = 1MB
$MaxBinaryBytes = 128MB
$MaxNoticeBytes = 16MB

function Test-AllowedRedirect([Uri]$Initial, [Uri]$Target) {
    if ($Target.Scheme -ne 'https' -or -not [string]::IsNullOrEmpty($Target.UserInfo)) { return $false }
    if ($Target.Authority -eq $Initial.Authority) { return $true }
    return $Initial.DnsSafeHost -eq 'github.com' -and $Target.Port -eq 443 -and ($Target.DnsSafeHost -eq 'githubusercontent.com' -or $Target.DnsSafeHost.EndsWith('.githubusercontent.com'))
}

function Invoke-SecureDownload([Uri]$Uri, [string]$Destination, [long]$MaximumBytes) {
    Add-Type -AssemblyName System.Net.Http
    $Handler = New-Object Net.Http.HttpClientHandler
    $Handler.AllowAutoRedirect = $false
    $Client = [Net.Http.HttpClient]::new($Handler)
    $Client.Timeout = [TimeSpan]::FromSeconds(30)
    $Client.DefaultRequestHeaders.UserAgent.ParseAdd('cvc-installer')
    $Initial = $Uri
    try {
        for ($Redirects = 0; $Redirects -le 5; $Redirects++) {
            $Response = $Client.GetAsync($Uri, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
            if ([int]$Response.StatusCode -in @(301, 302, 303, 307, 308)) {
                try {
                    if ($Redirects -eq 5 -or $null -eq $Response.Headers.Location) { throw "Too many or invalid redirects while downloading $Initial" }
                    $Target = if ($Response.Headers.Location.IsAbsoluteUri) { $Response.Headers.Location } else { New-Object Uri($Uri, $Response.Headers.Location) }
                    if (-not (Test-AllowedRedirect $Initial $Target)) { throw "Refusing cross-origin or insecure redirect to $($Target.GetLeftPart([UriPartial]::Authority))" }
                    $Uri = $Target
                } finally { $Response.Dispose() }
                continue
            }
            if (-not $Response.IsSuccessStatusCode) { try { throw "Download failed ($([int]$Response.StatusCode)) for $Initial" } finally { $Response.Dispose() } }
            try {
                if ($Response.Content.Headers.ContentLength -gt $MaximumBytes) { throw "Release download is unexpectedly large." }
                $InputStream = $Response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                $OutputStream = New-Object IO.FileStream($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $Buffer = New-Object byte[] 65536
                    [long]$Total = 0
                    while (($Read = $InputStream.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
                        $Total += $Read
                        if ($Total -gt $MaximumBytes) { throw "Release download is unexpectedly large." }
                        $OutputStream.Write($Buffer, 0, $Read)
                    }
                    $OutputStream.Flush($true)
                } finally { $OutputStream.Dispose(); $InputStream.Dispose() }
            } finally { $Response.Dispose() }
            return
        }
    } finally { $Client.Dispose(); $Handler.Dispose() }
}

function Expand-SafeBinary([string]$Archive, [string]$Member, [string]$Destination) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $Zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $Entries = @($Zip.Entries | Where-Object { $_.FullName -ceq $Member })
        if ($Entries.Count -ne 1 -or $Entries[0].Length -le 0 -or $Entries[0].Length -gt $MaxBinaryBytes) { throw "Release archive contains an unsafe or missing binary: $Member" }
        $InputStream = $Entries[0].Open()
        $OutputStream = New-Object IO.FileStream($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try {
            $Buffer = New-Object byte[] 65536
            [long]$Total = 0
            while (($Read = $InputStream.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
                $Total += $Read
                if ($Total -gt $MaxBinaryBytes) { throw "Expanded binary is unexpectedly large: $Member" }
                $OutputStream.Write($Buffer, 0, $Read)
            }
            $OutputStream.Flush($true)
        }
        finally { $OutputStream.Dispose(); $InputStream.Dispose() }
    } finally { $Zip.Dispose() }
}

# Like Expand-SafeBinary, for the optional license and third-party notices:
# $true once the archive's one entry named $Member (not empty, at most
# $MaximumBytes) has been written to $Destination, a new file. $false, with no
# file left at $Destination, when the archive has no such entry or it cannot be
# extracted within those limits. It does not throw for either: a release that
# predates the notices is installed without them.
function Expand-OptionalNotice([string]$Archive, [string]$Member, [string]$Destination, [long]$MaximumBytes) {
    $Staged = $false
    $Created = $false
    try {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $Zip = [IO.Compression.ZipFile]::OpenRead($Archive)
        try {
            $Entries = @($Zip.Entries | Where-Object { $_.FullName -ceq $Member })
            if ($Entries.Count -eq 1 -and $Entries[0].Length -gt 0 -and $Entries[0].Length -le $MaximumBytes) {
                $InputStream = $Entries[0].Open()
                $OutputStream = New-Object IO.FileStream($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
                $Created = $true
                try {
                    $Buffer = New-Object byte[] 65536
                    [long]$Total = 0
                    while (($Read = $InputStream.Read($Buffer, 0, $Buffer.Length)) -gt 0) {
                        $Total += $Read
                        if ($Total -gt $MaximumBytes) { throw "Expanded notice is unexpectedly large: $Member" }
                        $OutputStream.Write($Buffer, 0, $Read)
                    }
                    $OutputStream.Flush($true)
                    $Staged = $Total -gt 0
                }
                finally { $OutputStream.Dispose(); $InputStream.Dispose() }
            }
        } finally { $Zip.Dispose() }
    } catch { $Staged = $false }
    if ((-not $Staged) -and $Created) { Remove-Item -LiteralPath $Destination -Force -ErrorAction SilentlyContinue }
    return $Staged
}

# 'absent', 'file' (a regular file), or 'other' (a directory or any reparse
# point, which are never replaced or deleted).
function Get-CvcPathKind([string]$Path) {
    $Item = Get-Item -LiteralPath $Path -Force -ErrorAction SilentlyContinue
    if ($null -eq $Item) { return 'absent' }
    if ($Item.PSIsContainer -or ($Item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { return 'other' }
    return 'file'
}

# `.cvc-old-<file>-<16 lowercase hex>`, the name the npm launcher
# (npm/cvc-mcp/bin/release.js) gives a replaced binary, so that either
# publisher sweeps up the other's leftovers.
function New-CvcRetiredPath([string]$InstallDir, [string]$Binary) {
    $Bytes = New-Object byte[] 8
    $Generator = [Security.Cryptography.RandomNumberGenerator]::Create()
    try { $Generator.GetBytes($Bytes) } finally { $Generator.Dispose() }
    return Join-Path $InstallDir ('.cvc-old-' + $Binary + '-' + [BitConverter]::ToString($Bytes).Replace('-', '').ToLowerInvariant())
}

# Deletes leftovers of earlier publishes, best effort: one that is still running
# cannot be deleted and is retried by the next publish. Only regular files with
# exactly the retired-image name are touched, never directories or reparse points.
function Remove-CvcRetiredImages([string]$InstallDir) {
    foreach ($Entry in @(Get-ChildItem -LiteralPath $InstallDir -Force -Filter '.cvc-old-*' -ErrorAction SilentlyContinue)) {
        if ($Entry.Name -cnotmatch '^\.cvc-old-(cvc|cvc-mcp|cvc-lsp)(\.exe)?-[0-9a-f]{16}$') { continue }
        if ($Entry.PSIsContainer -or ($Entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) { continue }
        try { [IO.File]::Delete($Entry.FullName) } catch { }
    }
}

# Moves the prepared files (a hashtable of file name to a staged file in
# $InstallDir, so the move stays on one volume) over the installed binaries, in
# $MoveOrder. Windows refuses to overwrite or delete a running .exe but allows
# renaming it, and the printed MCP config and the npm launcher both run
# cvc-mcp.exe from this directory. So an existing target is renamed aside, the
# new file is moved in, the old one is renamed back if that fails, and the retired
# file is deleted best effort (it stays while its process runs). Throws on
# failure; nothing is replaced when a target or a prepared file is unusable.
function Publish-CvcBinaries([string]$InstallDir, [string[]]$MoveOrder, [hashtable]$Prepared) {
    Remove-CvcRetiredImages $InstallDir
    foreach ($Binary in $MoveOrder) {
        $Target = Join-Path $InstallDir $Binary
        if ((Get-CvcPathKind $Target) -eq 'other') { throw "Refusing unsafe existing target: $Target" }
        if (-not $Prepared.ContainsKey($Binary) -or (Get-CvcPathKind $Prepared[$Binary]) -ne 'file') { throw "The prepared file for $Binary is missing or unsafe." }
    }
    foreach ($Binary in $MoveOrder) {
        $Target = Join-Path $InstallDir $Binary
        $Retired = $null
        $Kind = Get-CvcPathKind $Target
        if ($Kind -eq 'other') { throw "Refusing unsafe existing target: $Target" }
        if ($Kind -eq 'file') {
            $Retired = New-CvcRetiredPath $InstallDir $Binary
            [IO.File]::Move($Target, $Retired)
        }
        try { [IO.File]::Move($Prepared[$Binary], $Target) }
        catch {
            $Failure = $_
            if ($null -ne $Retired) {
                try { [IO.File]::Move($Retired, $Target) }
                catch { Write-Warning "Could not restore $Target; the previous file is $Retired" }
            }
            throw $Failure
        }
        if ($null -ne $Retired) { try { [IO.File]::Delete($Retired) } catch { } }
    }
}

# Installs the staged notices (a hashtable of file name to a staged file) into
# $ShareDir. The checks come first and are all or nothing: an unsafe directory or
# an unsafe existing target stops it before anything is replaced. Each file is
# copied into $ShareDir under a temporary name (so the final move stays on one
# volume) and then replaces the installed one, so a re-run leaves exactly one
# copy of each file. The notices are documentation, so this never throws: on any
# problem it warns and returns $false, and the install carries on without them.
function Publish-CvcNotices([string]$ShareDir, [hashtable]$Staged) {
    $Temporary = @{}
    try {
        if (Test-Path -LiteralPath $ShareDir) {
            $ShareItem = Get-Item -LiteralPath $ShareDir -Force
            if (-not $ShareItem.PSIsContainer -or ($ShareItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "not a safe directory: $ShareDir" }
        } else { New-Item -ItemType Directory -Path $ShareDir | Out-Null }
        foreach ($Name in @($Staged.Keys)) {
            $Target = Join-Path $ShareDir $Name
            if ((Get-CvcPathKind $Target) -eq 'other') { throw "refusing unsafe existing target: $Target" }
        }
        foreach ($Name in @($Staged.Keys)) {
            $Copy = Join-Path $ShareDir ('.cvc-' + $Name + '.' + [Guid]::NewGuid())
            $Temporary[$Name] = $Copy
            [IO.File]::Copy($Staged[$Name], $Copy, $false)
        }
        foreach ($Name in @($Staged.Keys)) {
            $Target = Join-Path $ShareDir $Name
            # The third argument is the backup file name, and "none" must be a real null
            # string: PowerShell turns $null into "" for a .NET string parameter, and
            # File.Replace rejects "" ("The path is not of a legal form").
            if ((Get-CvcPathKind $Target) -eq 'file') { [IO.File]::Replace($Temporary[$Name], $Target, [NullString]::Value) }
            else { [IO.File]::Move($Temporary[$Name], $Target) }
            $Temporary.Remove($Name)
        }
        return $true
    }
    catch {
        Write-Warning "The license and third-party notices were not installed: $($_.Exception.Message)"
        return $false
    }
    finally {
        foreach ($Copy in @($Temporary.Values)) { Remove-Item -LiteralPath $Copy -Force -ErrorAction SilentlyContinue }
    }
}

# Returns 'add' or 'skip'. Read-Host throws in a -NonInteractive host and
# misbehaves when stdin is redirected (CI, automation), so an unattended session
# skips PATH configuration instead of failing a successful install. Answering
# anything that starts with n or N also skips, as install.sh does.
function Read-CvcPathConsent([string]$Prompt) {
    $Interactive = $false
    try { $Interactive = [Environment]::UserInteractive -and -not [Console]::IsInputRedirected } catch { }
    if (-not $Interactive) { return 'skip' }
    try { $Answer = Read-Host $Prompt } catch { return 'skip' }
    if ($Answer -match '^\s*[nN]') { return 'skip' }
    return 'add'
}

Write-Host "Installing CVC..."
$Arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
if ($Arch -notin @("AMD64", "x86_64")) { throw "Unsupported architecture: $Arch. Windows releases are published for x64 only." }

$AssetName = "cvc-x86_64-pc-windows-msvc.zip"
$ReleaseUrl = if ($ReleaseVersion -eq "latest") {
    "$ReleaseBaseUrl/$Repo/releases/latest/download"
} else {
    "$ReleaseBaseUrl/$Repo/releases/download/$ReleaseVersion"
}
$ArchiveUrl = [Uri]"$ReleaseUrl/$AssetName"
$ChecksumUrl = [Uri]"$ReleaseUrl/SHA256SUMS.txt"
$TempDir = Join-Path ([IO.Path]::GetTempPath()) ("cvc-install-" + [Guid]::NewGuid())
$ArchivePath = Join-Path $TempDir $AssetName
$ChecksumPath = Join-Path $TempDir "SHA256SUMS"
$StageDir = Join-Path $TempDir "stage"
$Binaries = @("cvc.exe", "cvc-mcp.exe", "cvc-lsp.exe")
# cvc.exe moves into place LAST: the npm launcher probes `cvc --version`
# without the lock, so a new cvc must imply its siblings are already new.
$MoveOrder = @("cvc-lsp.exe", "cvc-mcp.exe", "cvc.exe")
$Prepared = @{}
# Optional: an older release archive may lack them, and a problem installing
# them never stops the binaries from being installed.
$NoticeFiles = @("LICENSE", "THIRD-PARTY-NOTICES.md")
$NoticesStaged = @{}
$NoticesAbsent = @()
$NoticesInstalled = @()
$NoticesFailed = @()
$InstallLock = $null
$InstallLockPath = $null
$InstallLockAcquired = $false

try {
    New-Item -ItemType Directory -Path $TempDir, $StageDir | Out-Null
    Write-Host "Downloading CVC release from $Repo..."
    Invoke-SecureDownload $ChecksumUrl $ChecksumPath $MaxChecksumBytes
    Invoke-SecureDownload $ArchiveUrl $ArchivePath $MaxArchiveBytes

    $ChecksumMatches = @([IO.File]::ReadAllLines($ChecksumPath) | Where-Object { $_ -match ('^([a-fA-F0-9]{64})\s+\*?' + [regex]::Escape($AssetName) + '$') })
    if ($ChecksumMatches.Count -ne 1) { throw "SHA256SUMS does not contain one valid checksum for $AssetName." }
    $Expected = ([regex]::Match($ChecksumMatches[0], '^[a-fA-F0-9]{64}')).Value.ToLowerInvariant()
    $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $ArchivePath).Hash.ToLowerInvariant()
    if ($Expected -cne $Actual) { throw "Checksum verification failed for $AssetName." }

    foreach ($Binary in $Binaries) { Expand-SafeBinary $ArchivePath $Binary (Join-Path $StageDir $Binary) }
    # LICENSE and THIRD-PARTY-NOTICES.md come out of the archive the same way,
    # but they are optional: an older release that lacks them is installed without.
    foreach ($Notice in $NoticeFiles) {
        $NoticePath = Join-Path $StageDir $Notice
        if (Expand-OptionalNotice $ArchivePath $Notice $NoticePath $MaxNoticeBytes) { $NoticesStaged[$Notice] = $NoticePath }
        else { $NoticesAbsent += $Notice }
    }
    if (Test-Path -LiteralPath $InstallDir) {
        $InstallItem = Get-Item -LiteralPath $InstallDir -Force
        if (-not $InstallItem.PSIsContainer -or ($InstallItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw "Refusing unsafe installation directory: $InstallDir" }
    } else { New-Item -ItemType Directory -Path $InstallDir | Out-Null }
    $InstallLockPath = Join-Path $InstallDir ".cvc-install.lock"
    try { $InstallLock = New-Object IO.FileStream($InstallLockPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None); $InstallLockAcquired = $true }
    catch { throw "Another installation is active (or a stale lock exists): $InstallLockPath" }

    foreach ($Binary in $Binaries) {
        $Temporary = Join-Path $InstallDir (".cvc-" + $Binary + "." + [Guid]::NewGuid())
        [IO.File]::Copy((Join-Path $StageDir $Binary), $Temporary, $false)
        $Prepared[$Binary] = $Temporary
    }
    # The notices go in under the same lock, after every binary has been staged
    # and before the first one is replaced, so a refused install has changed no
    # notice either. They cannot stop the binaries: a problem is warned about (see
    # Publish-CvcNotices) and reported in the summary.
    if ($NoticesStaged.Count -gt 0) {
        $NoticesAttempted = @($NoticeFiles | Where-Object { $NoticesStaged.ContainsKey($_) })
        if (Publish-CvcNotices -ShareDir $ShareDir -Staged $NoticesStaged) { $NoticesInstalled = $NoticesAttempted }
        else { $NoticesFailed = $NoticesAttempted }
    }
    # Refuses unsafe targets before replacing anything, then renames running
    # images aside instead of overwriting them.
    Publish-CvcBinaries -InstallDir $InstallDir -MoveOrder $MoveOrder -Prepared $Prepared
}
finally {
    if ($null -ne $InstallLock) { $InstallLock.Dispose() }
    if ($InstallLockAcquired) { Remove-Item -LiteralPath $InstallLockPath -Force -ErrorAction SilentlyContinue }
    if (Test-Path -LiteralPath $TempDir) { Remove-Item -LiteralPath $TempDir -Recurse -Force -ErrorAction SilentlyContinue }
    if ($null -ne $Prepared) { foreach ($Temporary in $Prepared.Values) { Remove-Item -LiteralPath $Temporary -Force -ErrorAction SilentlyContinue } }
}

Write-Host "`nSuccess! CVC installed to $InstallDir"
Write-Host "  cvc.exe       - CLI interface"
Write-Host "  cvc-mcp.exe   - MCP server for coding agents"
Write-Host "  cvc-lsp.exe   - Language server for the VSCode extension`n"
if ($NoticesInstalled.Count -gt 0) {
    Write-Host "License and third-party notices installed to $ShareDir"
    Write-Host "  $($NoticesInstalled -join ', ')`n"
}
if ($NoticesAbsent.Count -gt 0) { Write-Host "Not included in this release archive, so not installed: $($NoticesAbsent -join ', ')`n" }
if ($NoticesFailed.Count -gt 0) { Write-Host "Not installed (see the warning above): $($NoticesFailed -join ', ')`n" }
if ((Read-CvcPathConsent "Would you like to add CVC to your User PATH? [Y/n]") -eq 'add') {
    $UserPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($null -eq $UserPath) { $UserPath = '' }
    $PathEntries = @($UserPath -split ';' | Where-Object { $_ })
    if ($PathEntries -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable("PATH", (($UserPath.TrimEnd(';') + ';' + $InstallDir).TrimStart(';')), "User")
        Write-Host "Added $InstallDir to your User PATH. Restart your terminal to apply it."
    } else { Write-Host "CVC is already in your PATH." }
} else { Write-Host "Skipping PATH configuration. Add $InstallDir to your User PATH manually." }
Write-Host "`nMCP config: {`"cvc`": {`"command`": `"$InstallDir\cvc-mcp.exe`", `"args`": []}}"
