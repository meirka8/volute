# Offline checks for install.ps1, run by .github/workflows/test.yml on Windows
# under both Windows PowerShell 5.1 and PowerShell 7. They need no network.
#
# install.ps1 downloads a release when it runs, so it is never executed here.
# The script is parsed, and only the function definitions it needs are lifted
# out of the syntax tree and defined in this scope:
#   Publish-CvcBinaries    rename-aside replacement of installed binaries
#   Expand-OptionalNotice  staging LICENSE and THIRD-PARTY-NOTICES.md from the archive
#   Publish-CvcNotices     installing those two files beside the install directory
#   Read-CvcPathConsent    the PATH prompt that must not fail unattended
# Everything runs in a throwaway directory under %TEMP%, never in ~/.cvc/bin.
#
# Needs Windows: the running image is a copy of a system .exe, because Windows
# lets a running .exe be renamed but not overwritten or deleted, which is the
# behaviour the installer has to cope with.
#
# Keep this file ASCII: Windows PowerShell 5.1 reads a BOM-less file as ANSI.

$ErrorActionPreference = 'Stop'

if ($env:OS -ne 'Windows_NT') { throw 'test-install-ps1.ps1 needs Windows: it relies on running executables being locked.' }

$RepoRoot = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$InstallScript = Join-Path $RepoRoot 'install.ps1'
$TestRoot = Join-Path ([IO.Path]::GetTempPath()) ('cvc-install-ps1-test-' + [Guid]::NewGuid())
$Failures = New-Object 'System.Collections.Generic.List[string]'
$Started = New-Object 'System.Collections.Generic.List[System.Diagnostics.Process]'
# Junctions the cases create, removed one by one before the tree (see the cleanup).
$Junctions = New-Object 'System.Collections.Generic.List[string]'
$Checks = 0

# System executables with different contents: the "old" install, the "new"
# release, and a third one for the second publish.
$SystemDir = Join-Path $env:SystemRoot 'System32'
$OldSource = Join-Path $SystemDir 'PING.EXE'
$NewSource = Join-Path $SystemDir 'HOSTNAME.EXE'
$ThirdSource = Join-Path $SystemDir 'whoami.exe'

# The exact retired-image name shared with the npm launcher (release.js), and
# the name install.ps1 gives a file it has staged but not yet moved into place.
$RetiredName = '^\.cvc-old-(cvc|cvc-mcp|cvc-lsp)(\.exe)?-[0-9a-f]{16}$'
$StagedName = '^\.cvc-(cvc|cvc-mcp|cvc-lsp)\.exe\.'
# Look-alikes planted by the "running" case that happen to match $RetiredName
# but are not regular files (a directory, a junction, a symlink).
$NotRegular = @('.cvc-old-cvc-lsp.exe-aaaaaaaaaaaaaaaa', '.cvc-old-cvc.exe-bbbbbbbbbbbbbbbb', '.cvc-old-cvc-mcp.exe-cccccccccccccccc')

function Assert-Test([bool]$Condition, [string]$Message, [string]$Detail = '') {
    $script:Checks++
    if ($Condition) { Write-Host "  ok    $Message" }
    else {
        Write-Host "  FAIL  $Message"
        if ($Detail) { Write-Host "        $Detail" }
        $script:Failures.Add($Message)
    }
}

# install.ps1 tests some results with `if (...)`. A stray object in a function's
# output would turn its result into an array, which is always true, so the
# result must be exactly one boolean.
function Assert-Bool($Actual, [bool]$Expected, [string]$Message) {
    Assert-Test (($Actual -is [bool]) -and ($Actual -eq $Expected)) $Message ('result: ' + ($Actual | Out-String).Trim())
}

function Get-Sha([string]$Path) { return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash }

function New-CaseDir([string]$Name) {
    $Path = Join-Path $TestRoot $Name
    New-Item -ItemType Directory -Path $Path | Out-Null
    return $Path
}

# Stages a copy of $Source for each name, as install.ps1 does before publishing.
function New-StagedFiles([string]$Directory, [string]$Source, [string[]]$Names) {
    $Staged = @{}
    foreach ($Name in $Names) {
        $Path = Join-Path $Directory ('.cvc-' + $Name + '.' + [Guid]::NewGuid())
        [IO.File]::Copy($Source, $Path, $false)
        $Staged[$Name] = $Path
    }
    return $Staged
}

# Names in the directory that start like a retired image, whatever they are.
function Get-RetiredNames([string]$Directory) {
    return @(Get-ChildItem -LiteralPath $Directory -Force | Where-Object { $_.Name -cmatch '^\.cvc-old-' } | ForEach-Object { $_.Name })
}

# Returns $null when the publish succeeded, else the error message.
function Invoke-Publish([string]$Directory, [string[]]$Order, [hashtable]$Prepared) {
    try { Publish-CvcBinaries -InstallDir $Directory -MoveOrder $Order -Prepared $Prepared; return $null }
    catch { return [string]$_.Exception.Message }
}

# Starts a copy of a system executable that keeps running for a minute.
function Start-Sleeper([string]$Executable) {
    $Process = Start-Process -FilePath $Executable -ArgumentList @('-n', '60', '127.0.0.1') -WindowStyle Hidden -PassThru
    $script:Started.Add($Process)
    Start-Sleep -Milliseconds 700
    $Process.Refresh()
    if ($Process.HasExited) { throw "The test process $Executable exited early with code $($Process.ExitCode)." }
    return $Process
}

function Stop-Sleeper([System.Diagnostics.Process]$Process) {
    try { if (-not $Process.HasExited) { $Process.Kill() } } catch { }
    [void]$Process.WaitForExit(10000)
}

# True once nothing maps the file any more: a write handle can be opened on a
# file whose process has exited, but not on a running image.
function Wait-Unlocked([string]$Path) {
    $Deadline = [DateTime]::UtcNow.AddSeconds(10)
    while ([DateTime]::UtcNow -lt $Deadline) {
        try {
            $Stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Write, ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
            $Stream.Dispose()
            return $true
        } catch { Start-Sleep -Milliseconds 100 }
    }
    return $false
}

$ExitCode = 0
try {
    Write-Host "install.ps1 tests on $($PSVersionTable.PSEdition) $($PSVersionTable.PSVersion)"
    New-Item -ItemType Directory -Path $TestRoot | Out-Null
    foreach ($Source in @($OldSource, $NewSource, $ThirdSource)) {
        if (-not (Test-Path -LiteralPath $Source)) { throw "The system executable $Source is missing on this runner." }
    }
    $OldHash = Get-Sha $OldSource
    $NewHash = Get-Sha $NewSource
    $ThirdHash = Get-Sha $ThirdSource
    if (@(@($OldHash, $NewHash, $ThirdHash) | Select-Object -Unique).Count -ne 3) { throw 'The three system executables must differ.' }

    Write-Host 'install.ps1 syntax and the functions under test'
    $Tokens = $null
    $ParseErrors = $null
    $Ast = [System.Management.Automation.Language.Parser]::ParseFile($InstallScript, [ref]$Tokens, [ref]$ParseErrors)
    foreach ($Problem in $ParseErrors) { Write-Host "  parse error at line $($Problem.Extent.StartLineNumber): $($Problem.Message)" }
    Assert-Test (@($ParseErrors).Count -eq 0) 'install.ps1 parses without errors'
    if (@($ParseErrors).Count -gt 0) { throw 'install.ps1 does not parse.' }

    $FunctionAsts = @($Ast.FindAll({ param($Node) $Node -is [System.Management.Automation.Language.FunctionDefinitionAst] }, $false))
    $Needed = @('Get-CvcPathKind', 'New-CvcRetiredPath', 'Remove-CvcRetiredImages', 'Publish-CvcBinaries', 'Expand-OptionalNotice', 'Publish-CvcNotices', 'Read-CvcPathConsent')
    $ConsentSource = $null
    foreach ($Name in $Needed) {
        $Found = @($FunctionAsts | Where-Object { $_.Name -eq $Name })
        Assert-Test ($Found.Count -eq 1) "install.ps1 defines $Name exactly once"
        if ($Found.Count -ne 1) { throw "Cannot continue without $Name." }
        if ($Name -eq 'Read-CvcPathConsent') { $ConsentSource = $Found[0].Extent.Text }
        . ([scriptblock]::Create($Found[0].Extent.Text))
    }

    # The order the installer really uses, read from its source: cvc.exe last.
    $MoveAssignments = @($Ast.FindAll({ param($Node) ($Node -is [System.Management.Automation.Language.AssignmentStatementAst]) -and ($Node.Left -is [System.Management.Automation.Language.VariableExpressionAst]) -and ($Node.Left.VariablePath.UserPath -eq 'MoveOrder') }, $true))
    Assert-Test ($MoveAssignments.Count -eq 1) 'install.ps1 assigns $MoveOrder exactly once'
    if ($MoveAssignments.Count -ne 1) { throw 'Cannot continue without $MoveOrder.' }
    $MoveOrder = @($MoveAssignments[0].Right.FindAll({ param($Node) $Node -is [System.Management.Automation.Language.StringConstantExpressionAst] }, $true) | ForEach-Object { $_.Value })
    Assert-Test (($MoveOrder -join ',') -eq 'cvc-lsp.exe,cvc-mcp.exe,cvc.exe') 'the installer moves binaries in the order lsp, mcp, cvc (cvc.exe last)' ('found: ' + ($MoveOrder -join ', '))
    $Names = @('cvc.exe', 'cvc-mcp.exe', 'cvc-lsp.exe')

    # No prompt may bypass the function that guards it.
    $ReadHosts = @($Ast.FindAll({ param($Node) ($Node -is [System.Management.Automation.Language.CommandAst]) -and ($Node.GetCommandName() -eq 'Read-Host') }, $true))
    $Unguarded = @($ReadHosts | Where-Object {
        $Parent = $_.Parent
        while (($null -ne $Parent) -and -not ($Parent -is [System.Management.Automation.Language.FunctionDefinitionAst])) { $Parent = $Parent.Parent }
        ($null -eq $Parent) -or ($Parent.Name -ne 'Read-CvcPathConsent')
    })
    Assert-Test (($ReadHosts.Count -ge 1) -and ($Unguarded.Count -eq 0)) 'Read-Host is only called from inside Read-CvcPathConsent'

    # The notices go in once, before the binaries are replaced, so a refused
    # install has changed no notice either.
    $NoticeCalls = @($Ast.FindAll({ param($Node) ($Node -is [System.Management.Automation.Language.CommandAst]) -and ($Node.GetCommandName() -eq 'Publish-CvcNotices') }, $true))
    $BinaryCalls = @($Ast.FindAll({ param($Node) ($Node -is [System.Management.Automation.Language.CommandAst]) -and ($Node.GetCommandName() -eq 'Publish-CvcBinaries') }, $true))
    Assert-Test (($NoticeCalls.Count -eq 1) -and ($BinaryCalls.Count -eq 1) -and ($NoticeCalls[0].Extent.StartOffset -lt $BinaryCalls[0].Extent.StartOffset)) 'install.ps1 installs the notices once, before it replaces the binaries'

    Write-Host 'first install into an empty directory'
    $Dir = New-CaseDir 'fresh'
    $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
    $Result = Invoke-Publish $Dir $MoveOrder $Staged
    Assert-Test ($null -eq $Result) 'the publish succeeds' "$Result"
    foreach ($Name in $Names) { Assert-Test ((Get-Sha (Join-Path $Dir $Name)) -eq $NewHash) "$Name holds the new release" }
    Assert-Test (@(Get-RetiredNames $Dir).Count -eq 0) 'nothing is retired when nothing was replaced'
    Assert-Test (@(Get-ChildItem -LiteralPath $Dir -Force | Where-Object { $_.Name -cmatch $StagedName }).Count -eq 0) 'no staged file is left behind'

    Write-Host 'replacing binaries while cvc-mcp.exe is running'
    $Dir = New-CaseDir 'running'
    foreach ($Name in $Names) { [IO.File]::Copy($OldSource, (Join-Path $Dir $Name), $false) }
    $Sleeper = Start-Sleeper (Join-Path $Dir 'cvc-mcp.exe')
    # Leftovers to be swept: one named as the launcher names them, one without .exe.
    [IO.File]::WriteAllText((Join-Path $Dir '.cvc-old-cvc.exe-0123456789abcdef'), 'launcher leftover')
    [IO.File]::WriteAllText((Join-Path $Dir '.cvc-old-cvc-lsp-fedcba9876543210'), 'leftover without .exe')
    # Look-alikes that must survive because they do not match the name exactly.
    $Keep = @(
        '.cvc-old-notes.txt',
        '.cvc-old-cvc.exe-0F1E2D3C4B5A6978',
        '.cvc-old-cvc.exe-0123456789abcde',
        '.cvc-old-cvc.exe-0123456789abcdef0',
        '.cvc-old-other.exe-0123456789abcdef'
    )
    foreach ($Name in $Keep) { [IO.File]::WriteAllText((Join-Path $Dir $Name), 'keep me') }
    # ...and ones that match the name but are not regular files.
    $KeepDir = Join-Path $Dir '.cvc-old-cvc-lsp.exe-aaaaaaaaaaaaaaaa'
    New-Item -ItemType Directory -Path $KeepDir | Out-Null
    [IO.File]::WriteAllText((Join-Path $KeepDir 'inner.txt'), 'inside a look-alike directory')
    $JunctionTarget = New-CaseDir 'junction-target'
    [IO.File]::WriteAllText((Join-Path $JunctionTarget 'precious.txt'), 'must not be reached through a reparse point')
    $Junction = Join-Path $Dir '.cvc-old-cvc.exe-bbbbbbbbbbbbbbbb'
    $HaveJunction = $false
    try { New-Item -ItemType Junction -Path $Junction -Value $JunctionTarget | Out-Null; $Junctions.Add($Junction); $HaveJunction = $true } catch { Write-Host "  skip  junction look-alike: $($_.Exception.Message)" }
    $SymlinkTarget = Join-Path $TestRoot 'symlink-target.txt'
    [IO.File]::WriteAllText($SymlinkTarget, 'must not be deleted through a symlink')
    $Symlink = Join-Path $Dir '.cvc-old-cvc-mcp.exe-cccccccccccccccc'
    $HaveSymlink = $false
    try { New-Item -ItemType SymbolicLink -Path $Symlink -Value $SymlinkTarget | Out-Null; $HaveSymlink = $true } catch { Write-Host "  skip  symlink look-alike: $($_.Exception.Message)" }

    $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
    $Result = Invoke-Publish $Dir $MoveOrder $Staged
    Assert-Test ($null -eq $Result) 'the publish succeeds although cvc-mcp.exe is running' "$Result"
    foreach ($Name in $Names) { Assert-Test ((Get-Sha (Join-Path $Dir $Name)) -eq $NewHash) "$Name holds the new release" }
    $Sleeper.Refresh()
    Assert-Test (-not $Sleeper.HasExited) 'the running cvc-mcp.exe was not disturbed'
    $Present = @(Get-RetiredNames $Dir)
    $Fresh = @($Present | Where-Object { ($_ -cmatch $RetiredName) -and ($NotRegular -cnotcontains $_) })
    Assert-Test (($Fresh.Count -eq 1) -and ($Fresh[0] -cmatch '^\.cvc-old-cvc-mcp\.exe-')) 'exactly one retired image remains: the running cvc-mcp.exe (the old cvc.exe and cvc-lsp.exe were deleted)' ('retired images present: ' + ($Fresh -join ', '))
    $RetiredMcpPath = if ($Fresh.Count -ge 1) { Join-Path $Dir $Fresh[0] } else { $null }
    if (($null -ne $RetiredMcpPath) -and (Test-Path -LiteralPath $RetiredMcpPath)) {
        Assert-Test ((Get-Sha $RetiredMcpPath) -eq $OldHash) 'the retired file is the old, running image'
    }
    Assert-Test (-not ($Present -ccontains '.cvc-old-cvc.exe-0123456789abcdef')) 'the launcher-style leftover was swept'
    Assert-Test (-not ($Present -ccontains '.cvc-old-cvc-lsp-fedcba9876543210')) 'the leftover without .exe was swept'
    foreach ($Name in $Keep) { Assert-Test (Test-Path -LiteralPath (Join-Path $Dir $Name)) "the look-alike $Name was left alone" }
    Assert-Test (Test-Path -LiteralPath (Join-Path $KeepDir 'inner.txt')) 'a directory named like a retired image was left alone'
    if ($HaveJunction) {
        Assert-Test ((Test-Path -LiteralPath $Junction) -and (Test-Path -LiteralPath (Join-Path $JunctionTarget 'precious.txt'))) 'a junction named like a retired image was not followed or deleted'
    }
    if ($HaveSymlink) {
        Assert-Test ((Test-Path -LiteralPath $Symlink) -and (Test-Path -LiteralPath $SymlinkTarget)) 'a symlink named like a retired image was not followed or deleted'
    }
    Assert-Test (@(Get-ChildItem -LiteralPath $Dir -Force | Where-Object { $_.Name -cmatch $StagedName }).Count -eq 0) 'no staged file is left behind'

    Write-Host 'the next publish sweeps the retired image once its process has exited'
    Stop-Sleeper $Sleeper
    if ($null -ne $RetiredMcpPath) { Assert-Test (Wait-Unlocked $RetiredMcpPath) 'the retired image is released after its process exits' }
    $Staged = New-StagedFiles $Dir $ThirdSource $MoveOrder
    $Result = Invoke-Publish $Dir $MoveOrder $Staged
    Assert-Test ($null -eq $Result) 'the second publish succeeds' "$Result"
    foreach ($Name in $Names) { Assert-Test ((Get-Sha (Join-Path $Dir $Name)) -eq $ThirdHash) "$Name holds the second release" }
    $Present = @(Get-RetiredNames $Dir)
    $Fresh = @($Present | Where-Object { ($_ -cmatch $RetiredName) -and ($NotRegular -cnotcontains $_) })
    Assert-Test ($Fresh.Count -eq 0) 'no retired image is left: the first publish''s cvc-mcp.exe was swept' ('retired images present: ' + ($Fresh -join ', '))
    Assert-Test (Test-Path -LiteralPath (Join-Path $Dir '.cvc-old-notes.txt')) 'the look-alike is still left alone'

    Write-Host 'a failed move restores the original and leaves cvc.exe untouched'
    $Dir = New-CaseDir 'restore'
    foreach ($Name in $Names) { [IO.File]::Copy($OldSource, (Join-Path $Dir $Name), $false) }
    $Sleeper = Start-Sleeper (Join-Path $Dir 'cvc-mcp.exe')
    $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
    # An exclusive handle on the staged cvc-mcp.exe makes moving it fail after
    # the installed one has been renamed aside.
    $Hold = [IO.File]::Open($Staged['cvc-mcp.exe'], [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::None)
    try { $Result = Invoke-Publish $Dir $MoveOrder $Staged } finally { $Hold.Dispose() }
    Assert-Test ($null -ne $Result) 'the publish reports the failure'
    Assert-Test ((Get-Sha (Join-Path $Dir 'cvc-mcp.exe')) -eq $OldHash) 'the original cvc-mcp.exe is restored'
    Assert-Test ((Get-Sha (Join-Path $Dir 'cvc.exe')) -eq $OldHash) 'cvc.exe was not replaced because a sibling failed (cvc.exe goes last)'
    $Sleeper.Refresh()
    Assert-Test (-not $Sleeper.HasExited) 'the running cvc-mcp.exe was not disturbed'
    Assert-Test (@(Get-RetiredNames $Dir | Where-Object { $_ -cmatch 'cvc-mcp' }).Count -eq 0) 'no retired cvc-mcp.exe is left after the restore'
    Stop-Sleeper $Sleeper

    Write-Host 'a missing prepared file is refused before anything is replaced'
    $Dir = New-CaseDir 'missing'
    foreach ($Name in $Names) { [IO.File]::Copy($OldSource, (Join-Path $Dir $Name), $false) }
    $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
    Remove-Item -LiteralPath $Staged['cvc-mcp.exe'] -Force
    $Result = Invoke-Publish $Dir $MoveOrder $Staged
    Assert-Test ($null -ne $Result) 'the publish reports the failure'
    foreach ($Name in $Names) { Assert-Test ((Get-Sha (Join-Path $Dir $Name)) -eq $OldHash) "$Name is unchanged" }
    Assert-Test (@(Get-RetiredNames $Dir).Count -eq 0) 'nothing was renamed aside'

    Write-Host 'an unsafe existing target is refused before anything is replaced'
    $Dir = New-CaseDir 'unsafe'
    [IO.File]::Copy($OldSource, (Join-Path $Dir 'cvc.exe'), $false)
    [IO.File]::Copy($OldSource, (Join-Path $Dir 'cvc-mcp.exe'), $false)
    New-Item -ItemType Directory -Path (Join-Path $Dir 'cvc-lsp.exe') | Out-Null
    $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
    $Result = Invoke-Publish $Dir $MoveOrder $Staged
    Assert-Test (($null -ne $Result) -and ($Result -match 'Refusing unsafe existing target')) 'a directory in place of cvc-lsp.exe is refused' "$Result"
    Assert-Test (((Get-Sha (Join-Path $Dir 'cvc.exe')) -eq $OldHash) -and ((Get-Sha (Join-Path $Dir 'cvc-mcp.exe')) -eq $OldHash)) 'the other binaries are unchanged'
    Assert-Test (Test-Path -LiteralPath (Join-Path $Dir 'cvc-lsp.exe') -PathType Container) 'the directory is still there'
    Assert-Test (@(Get-RetiredNames $Dir).Count -eq 0) 'nothing was renamed aside'
    if ($HaveSymlink) {
        $Dir = New-CaseDir 'unsafe-link'
        [IO.File]::Copy($OldSource, (Join-Path $Dir 'cvc.exe'), $false)
        [IO.File]::Copy($OldSource, (Join-Path $Dir 'cvc-lsp.exe'), $false)
        $LinkTarget = Join-Path $TestRoot 'link-target.exe'
        [IO.File]::Copy($OldSource, $LinkTarget, $false)
        New-Item -ItemType SymbolicLink -Path (Join-Path $Dir 'cvc-mcp.exe') -Value $LinkTarget | Out-Null
        $Staged = New-StagedFiles $Dir $NewSource $MoveOrder
        $Result = Invoke-Publish $Dir $MoveOrder $Staged
        Assert-Test (($null -ne $Result) -and ($Result -match 'Refusing unsafe existing target')) 'a symlink in place of cvc-mcp.exe is refused' "$Result"
        Assert-Test (((Get-Sha $LinkTarget) -eq $OldHash) -and ((Get-Sha (Join-Path $Dir 'cvc-lsp.exe')) -eq $OldHash)) 'nothing was written through or around the link'
    }

    Write-Host 'the PATH prompt skips instead of failing when input is redirected'
    # A child process with piped stdin, as in CI or automation. Read-Host would
    # throw or consume the piped text; the function must answer 'skip' instead.
    $ChildScript = Join-Path $TestRoot 'consent-child.ps1'
    $ChildLines = @($ConsentSource, "Write-Output ('RESULT=' + (Read-CvcPathConsent 'Add to PATH? [Y/n]'))")
    Set-Content -LiteralPath $ChildScript -Value $ChildLines -Encoding UTF8
    $HostExe = (Get-Process -Id $PID).Path
    $ChildOutput = 'y' | & $HostExe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $ChildScript
    $ChildExit = $LASTEXITCODE
    $ChildText = ($ChildOutput | Out-String).Trim()
    Assert-Test ($ChildExit -eq 0) 'the prompt child process exits 0' "exit code $ChildExit"
    Assert-Test ($ChildText -eq 'RESULT=skip') 'Read-CvcPathConsent returns skip' "output: $ChildText"

    Write-Host 'optional notices are staged from the archive only when it holds them'
    $NoticeSources = New-CaseDir 'notice-sources'
    [IO.File]::WriteAllText((Join-Path $NoticeSources 'LICENSE'), "license text`n")
    [IO.File]::WriteAllText((Join-Path $NoticeSources 'THIRD-PARTY-NOTICES.md'), "# third-party notices`n")
    [IO.File]::Copy($OldSource, (Join-Path $NoticeSources 'cvc.exe'), $false)
    $ZipFull = Join-Path $TestRoot 'notices-full.zip'
    $ZipBare = Join-Path $TestRoot 'notices-bare.zip'
    $ZipEmpty = Join-Path $TestRoot 'notices-empty.zip'
    Compress-Archive -LiteralPath (Join-Path $NoticeSources 'cvc.exe'), (Join-Path $NoticeSources 'LICENSE'), (Join-Path $NoticeSources 'THIRD-PARTY-NOTICES.md') -DestinationPath $ZipFull
    Compress-Archive -LiteralPath (Join-Path $NoticeSources 'cvc.exe') -DestinationPath $ZipBare
    # An archive whose LICENSE entry has no content.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $EmptyZip = [IO.Compression.ZipFile]::Open($ZipEmpty, 'Create')
    try { [void]$EmptyZip.CreateEntry('LICENSE') } finally { $EmptyZip.Dispose() }
    $NoticeOut = New-CaseDir 'notice-staged'
    $NoticeLimit = 1MB
    $Dest = Join-Path $NoticeOut 'LICENSE'
    Assert-Bool (Expand-OptionalNotice $ZipFull 'LICENSE' $Dest $NoticeLimit) $true 'a notice the archive holds is staged'
    Assert-Test ((Test-Path -LiteralPath $Dest) -and ((Get-Sha $Dest) -eq (Get-Sha (Join-Path $NoticeSources 'LICENSE')))) 'the staged notice has the content of the archive entry'
    $Dest = Join-Path $NoticeOut 'missing'
    Assert-Bool (Expand-OptionalNotice $ZipBare 'LICENSE' $Dest $NoticeLimit) $false 'an archive without the notice reports it as absent, without throwing'
    Assert-Test (-not (Test-Path -LiteralPath $Dest)) 'and leaves no file behind'
    $Dest = Join-Path $NoticeOut 'too-big'
    Assert-Bool (Expand-OptionalNotice $ZipFull 'THIRD-PARTY-NOTICES.md' $Dest 4) $false 'a notice over the size limit is not staged'
    Assert-Test (-not (Test-Path -LiteralPath $Dest)) 'and leaves no file behind'
    $Dest = Join-Path $NoticeOut 'empty'
    Assert-Bool (Expand-OptionalNotice $ZipEmpty 'LICENSE' $Dest $NoticeLimit) $false 'an empty notice is not staged'
    Assert-Test (-not (Test-Path -LiteralPath $Dest)) 'and leaves no file behind'

    Write-Host 'notices are installed beside the install directory and replaced in place'
    $NoticeStaged = @{ 'LICENSE' = (Join-Path $NoticeSources 'LICENSE'); 'THIRD-PARTY-NOTICES.md' = (Join-Path $NoticeSources 'THIRD-PARTY-NOTICES.md') }
    $ShareDir = Join-Path (Join-Path (New-CaseDir 'notice-prefix') 'share') 'cvc'
    $Result = Publish-CvcNotices -ShareDir $ShareDir -Staged $NoticeStaged
    Assert-Bool $Result $true 'the first install succeeds and creates the share directory'
    $Held = @(Get-ChildItem -LiteralPath $ShareDir -Force | ForEach-Object { $_.Name } | Sort-Object)
    Assert-Test (($Held -join ',') -eq 'LICENSE,THIRD-PARTY-NOTICES.md') 'exactly the two notice files are there, and no temporary copy' ('found: ' + ($Held -join ', '))
    Assert-Test ((Get-Sha (Join-Path $ShareDir 'LICENSE')) -eq (Get-Sha (Join-Path $NoticeSources 'LICENSE'))) 'LICENSE has the staged content'
    [IO.File]::WriteAllText((Join-Path $NoticeSources 'LICENSE'), "license text, newer`n")
    $Result = Publish-CvcNotices -ShareDir $ShareDir -Staged $NoticeStaged
    Assert-Bool $Result $true 'a re-run succeeds'
    $Held = @(Get-ChildItem -LiteralPath $ShareDir -Force | ForEach-Object { $_.Name } | Sort-Object)
    Assert-Test (($Held -join ',') -eq 'LICENSE,THIRD-PARTY-NOTICES.md') 'still exactly one copy of each notice, and no temporary copy' ('found: ' + ($Held -join ', '))
    Assert-Test ((Get-Sha (Join-Path $ShareDir 'LICENSE')) -eq (Get-Sha (Join-Path $NoticeSources 'LICENSE'))) 'LICENSE was replaced in place with the new content'

    Write-Host 'an unsafe notices destination is refused with a warning and nothing is replaced'
    $ShareDir = Join-Path (Join-Path (New-CaseDir 'notice-unsafe') 'share') 'cvc'
    $LicenseAsDirectory = Join-Path $ShareDir 'LICENSE'
    New-Item -ItemType Directory -Path $LicenseAsDirectory | Out-Null
    [IO.File]::WriteAllText((Join-Path $LicenseAsDirectory 'keep.txt'), 'keep')
    $Result = Publish-CvcNotices -ShareDir $ShareDir -Staged $NoticeStaged 3>$null
    Assert-Bool $Result $false 'a directory in place of LICENSE is refused'
    Assert-Test (Test-Path -LiteralPath (Join-Path $LicenseAsDirectory 'keep.txt')) 'the directory is untouched'
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $ShareDir 'THIRD-PARTY-NOTICES.md'))) 'the other notice was not installed either'
    Assert-Test (@(Get-ChildItem -LiteralPath $ShareDir -Force | Where-Object { $_.Name -cmatch '^\.cvc-' }).Count -eq 0) 'no temporary copy is left behind'
    $BlockedPrefix = New-CaseDir 'notice-blocked'
    [IO.File]::WriteAllText((Join-Path $BlockedPrefix 'share'), 'a file where the directory should be')
    $Result = Publish-CvcNotices -ShareDir (Join-Path (Join-Path $BlockedPrefix 'share') 'cvc') -Staged $NoticeStaged 3>$null
    Assert-Bool $Result $false 'a share path blocked by a file is refused without throwing'
    $LinkPrefix = New-CaseDir 'notice-link'
    $LinkTarget = New-CaseDir 'notice-link-target'
    New-Item -ItemType Directory -Path (Join-Path $LinkPrefix 'share') | Out-Null
    $ShareLink = Join-Path (Join-Path $LinkPrefix 'share') 'cvc'
    $HaveShareLink = $false
    try { New-Item -ItemType Junction -Path $ShareLink -Value $LinkTarget | Out-Null; $Junctions.Add($ShareLink); $HaveShareLink = $true } catch { Write-Host "  skip  junction as the share directory: $($_.Exception.Message)" }
    if ($HaveShareLink) {
        $Result = Publish-CvcNotices -ShareDir $ShareLink -Staged $NoticeStaged 3>$null
        Assert-Bool $Result $false 'a junction in place of the share directory is refused'
        Assert-Test (@(Get-ChildItem -LiteralPath $LinkTarget -Force).Count -eq 0) 'nothing was written through the junction'
    }
}
catch {
    Write-Host "ERROR: $($_.Exception.Message)"
    Write-Host $_.ScriptStackTrace
    $ExitCode = 1
}
finally {
    foreach ($Process in $Started) { Stop-Sleeper $Process }
    # A recursive delete over a junction fails under both shells ("The parameter is
    # incorrect"), so the cleanup below reported that it could not remove the tree.
    # Each junction is deleted by itself first: a non-recursive delete removes only
    # the link and never reaches what it points to.
    foreach ($Link in $Junctions) { try { [IO.Directory]::Delete($Link) } catch { } }
    try { [IO.Directory]::Delete($TestRoot, $true) } catch { Write-Host "  note  could not remove ${TestRoot}: $($_.Exception.Message)" }
}

if ($Failures.Count -gt 0) {
    Write-Host ''
    Write-Host "$($Failures.Count) of $Checks checks FAILED:"
    foreach ($Failure in $Failures) { Write-Host "  - $Failure" }
    $ExitCode = 1
}
if ($ExitCode -eq 0) { Write-Host "All $Checks checks passed." }
exit $ExitCode
