<#
.SYNOPSIS
Installs Personal Teams Assistant on Windows from this repository's prebuilt release archive,
for the current user only: no administrator rights, no WSL, no Rust.

.DESCRIPTION
The command the portal shows (Settings > Machines > Connect this computer):

  Invoke-RestMethod -UseBasicParsing https://github.com/elvisbrevi/personal-teams-assistant/releases/latest/download/install.ps1 -OutFile "$env:TEMP\pta-install.ps1"
  powershell -NoProfile -ExecutionPolicy Bypass -File "$env:TEMP\pta-install.ps1" -Origin https://your-portal.example

In order: detects the target; resolves the version (the repository's latest release unless
-Version is given); downloads that release's archive and its .sha256 over HTTPS from the release,
following only the release host's own redirect to its asset storage; checks the digest before
anything is unpacked or run; verifies build provenance with `gh attestation verify` when gh is
installed and signed in; unpacks into a temporary directory inside the installation root and moves
it into place (a failure leaves the previous installation working); writes the archive
installation record; adds the bin directory to the user PATH through the registry (never setx);
registers the host to start at login (`pta service install`, a scheduled task, no console window);
and, with -Origin, starts the browser sign-in so the approval page opens for "Trust this machine".

The command carries no secret: only the coordinator's public HTTPS origin. Running it again
repairs or upgrades the same installation; the profile (settings, credentials, Microsoft session,
machine identity) is never created, replaced or removed by this script.

.PARAMETER Origin
Your coordinator portal's HTTPS origin: after installing, start the browser sign-in.
.PARAMETER Version
Install this release instead of the latest one.
.PARAMETER Root
Installation root (default: %LOCALAPPDATA%\Programs\personal-teams-assistant).
.PARAMETER Integrations
Also install the skill and register `pta mcp` with the coding CLIs found; otherwise the commands
are printed.
.PARAMETER UnattendedUpdates
Let this computer install newer releases by itself when nothing would be interrupted (off unless
you pass this; change it later with `pta update unattended on|off --confirmed` or the local page).
.PARAMETER NoService
Do not register the host to start at login.
.PARAMETER NoBrowser
Print the approval link instead of opening the browser.
.PARAMETER Uninstall
Remove the service entry, the binaries and the PATH entry; keep the profile.
.PARAMETER TestSource
Only for the installer's own tests, with PTA_INSTALL_TEST=1: a local directory instead of the release.
#>
[CmdletBinding()]
param(
    [string] $Origin = '',
    [string] $Version = '',
    [string] $Root = '',
    [switch] $Integrations,
    [switch] $UnattendedUpdates,
    [switch] $NoService,
    [switch] $NoBrowser,
    [switch] $Uninstall,
    [string] $TestSource = ''
)

Set-StrictMode -Version 1.0
# The switches as plain script values, read by the functions below.
$WantIntegrations = $Integrations.IsPresent
$WantUnattended = $UnattendedUpdates.IsPresent
$SkipService = $NoService.IsPresent
$SkipBrowser = $NoBrowser.IsPresent
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Repository = 'elvisbrevi/personal-teams-assistant'
$Package = 'personal-teams-assistant'
$Cli = 'pta'
$Record = 'install.json'
$Target = 'x86_64-pc-windows-msvc'

function Say([string] $Text) { Write-Host $Text }
function Warn([string] $Text) { Write-Warning $Text }
function Fail([string] $Text) { throw "install.ps1: $Text" }

# ---------------------------------------------------------------------------------------------
# Inputs.

# Validated as the host validates a coordinator: https, a host, no credentials, path, query or
# fragment. Returns the normalized origin.
function Test-Origin([string] $Value) {
    $uri = $null
    if (-not [Uri]::TryCreate($Value, [UriKind]::Absolute, [ref] $uri) -or $uri.Scheme -ne 'https' -or
        $uri.UserInfo -or ($uri.AbsolutePath -ne '/') -or $uri.Query -or $uri.Fragment -or
        -not $uri.Host -or $Value -match '\s') {
        Fail 'Use the HTTPS origin of your coordinator portal, for example https://assistant.example.com.'
    }
    $origin = 'https://' + $uri.Host.ToLowerInvariant()
    if (-not $uri.IsDefaultPort) { $origin += ':' + $uri.Port }
    return $origin
}

function Test-Version([string] $Value) {
    if ($Value -notmatch '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        Fail 'Use a release version such as 0.11.4.'
    }
}

function Get-Architecture {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    return $arch
}

# ---------------------------------------------------------------------------------------------
# Downloads. A release asset is served by github.com with one redirect to its own asset storage;
# only that redirect is followed, and only over HTTPS. The test source is a local directory that
# only PTA_INSTALL_TEST=1 with -TestSource enables; the portal's command never sets either.

$AssetHosts = @('objects.githubusercontent.com', 'release-assets.githubusercontent.com', 'github-releases.githubusercontent.com')

function Get-Redirect([string] $Url) {
    $request = [System.Net.HttpWebRequest]::Create($Url)
    $request.AllowAutoRedirect = $false
    $request.Method = 'GET'
    $request.UserAgent = "$Package-installer"
    try {
        $response = $request.GetResponse()
    } catch [System.Net.WebException] {
        if ($null -eq $_.Exception.Response) { return $null }
        $response = $_.Exception.Response
    }
    try {
        $code = [int] $response.StatusCode
        if ($code -ge 300 -and $code -lt 400) { return [string] $response.Headers['Location'] }
        return $null
    } finally {
        $response.Close()
    }
}

function Get-Asset([string] $Name, [string] $Destination) {
    if ($TestSource) {
        $source = Join-Path $TestSource $Name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { return $false }
        Copy-Item -LiteralPath $source -Destination $Destination
        return $true
    }
    $location = Get-Redirect "https://github.com/$Repository/releases/download/v$Version/$Name"
    if (-not $location) { return $false }
    $uri = [Uri] $location
    if ($uri.Scheme -ne 'https' -or $AssetHosts -notcontains $uri.Host) {
        Fail 'The release host redirected to an unexpected address; nothing was downloaded.'
    }
    $client = New-Object System.Net.WebClient
    try {
        $client.Headers['User-Agent'] = "$Package-installer"
        $client.DownloadFile($uri, $Destination)
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
    return $true
}

function Get-LatestVersion {
    $location = Get-Redirect "https://github.com/$Repository/releases/latest"
    $prefix = "https://github.com/$Repository/releases/tag/v"
    if (-not $location -or -not $location.StartsWith($prefix)) {
        Fail 'The latest release could not be found. Check the network connection, or pass -Version.'
    }
    return $location.Substring($prefix.Length)
}

# ---------------------------------------------------------------------------------------------
# What is already on this computer.

function Test-CargoRecord([string] $Dir) {
    foreach ($name in '.crates2.json', '.crates.toml') {
        $file = Join-Path $Dir $name
        if ((Test-Path -LiteralPath $file -PathType Leaf) -and
            (Select-String -LiteralPath $file -SimpleMatch "`"$Package " -Quiet)) { return $true }
    }
    return $false
}

# A Cargo installation of this package would be a second copy: two hosts for one profile, updated
# two ways. Refuse and say how to switch.
function Assert-NoCargoCopy {
    $found = $null
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    if (Test-CargoRecord $cargoHome) { $found = $cargoHome }
    $onPath = Get-Command "$Cli.exe" -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($onPath -and ($onPath.Source -ne (Join-Path $Bin "$Cli.exe"))) {
        $other = Split-Path (Split-Path $onPath.Source -Parent) -Parent
        if (Test-CargoRecord $other) { $found = $other }
        else { Warn "Another $Cli is on your PATH at $($onPath.Source); the one installed here may be shadowed by it." }
    }
    if (-not $found) { return }
    Fail @"
$Package is already installed with Cargo in $found.
Installing a second copy next to it would give one profile two hosts. To switch to this
installation, which needs no Rust toolchain, run:

    pta service uninstall --confirmed
    pta app quit
    cargo uninstall $Package

and then this command again. Your profile, credentials and Microsoft session are kept.
To stay with Cargo instead, update with: cargo install $Package --locked
"@
}

# `cargo uninstall` leaves binstall's own record (crates-v1.json, one JSON object per line) naming
# the package. After a successful install, with no Cargo installation left, drop only this
# package's lines when the file is exactly that format, writing it atomically; otherwise say so.
function Clear-StaleBinstallRecord {
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
    $file = Join-Path $cargoHome 'binstall\crates-v1.json'
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { return }
    $utf8 = New-Object System.Text.UTF8Encoding $false
    $text = [System.IO.File]::ReadAllText($file, $utf8)
    $prefix = "{`"name`":`"$Package`""
    if (-not $text.Contains($prefix)) { return }
    if (Test-CargoRecord $cargoHome) { return }
    $lines = $text -split "`n"
    foreach ($line in $lines) {
        $bare = $line.TrimEnd("`r")
        if ($bare.Trim() -and -not ($bare.StartsWith('{"name":"') -and $bare.EndsWith('}'))) {
            Warn "binstall's record $file still names $Package, which is no longer installed with Cargo; delete its line by hand."
            return
        }
    }
    $kept = @($lines | Where-Object { -not $_.StartsWith($prefix) })
    $temp = "$file.pta-tmp"
    try {
        [System.IO.File]::WriteAllText($temp, ($kept -join "`n"), $utf8)
        Move-Item -LiteralPath $temp -Destination $file -Force
        Say "Removed the stale cargo binstall entry for $Package from $file."
    } catch {
        Remove-Item -LiteralPath $temp -Force -ErrorAction SilentlyContinue
        Warn "The stale cargo binstall entry for $Package in $file could not be removed; delete that line by hand."
    }
}

function Get-ProfileDir {
    if ($env:PTA_PROFILE_DIR) { return $env:PTA_PROFILE_DIR }
    return Join-Path $env:APPDATA 'dev.personalteams.assistant'
}

# ---------------------------------------------------------------------------------------------
# The host. Every pta call reads no input, and its JSON reply is parsed, never executed.

# Windows PowerShell 5.1 turns a native program's standard error into a terminating error under
# 'Stop'; native calls run with 'Continue' and are judged by their exit code.
function Invoke-Native {
    $ErrorActionPreference = 'Continue'
    $program, $rest = $args
    $output = '' | & $program @rest 2>&1 | Out-String
    return [pscustomobject] @{ Code = $LASTEXITCODE; Text = $output.Trim() }
}

function Test-GhSignedIn {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) { return $false }
    return (Invoke-Native gh auth status).Code -eq 0
}

function Invoke-Pta {
    $ErrorActionPreference = 'Continue'
    $exe = Join-Path $Bin "$Cli.exe"
    # Standard output only: a JSON reply is one line there, and notices go to standard error.
    $output = '' | & $exe @args 2>$null | Out-String
    return [pscustomobject] @{ Code = $LASTEXITCODE; Text = $output.Trim() }
}

function Get-PtaReply {
    $result = Invoke-Pta '--json' @args
    try { return $result.Text | ConvertFrom-Json } catch { return $null }
}

function Test-HostRunning {
    if (-not (Test-Path -LiteralPath (Join-Path $Bin "$Cli.exe"))) { return $false }
    $reply = Get-PtaReply 'status'
    return [bool] ($reply -and $reply.data -and $reply.data.host_running)
}

function Wait-Host([bool] $Running) {
    for ($i = 0; $i -lt 60; $i++) {
        if ((Test-HostRunning) -eq $Running) { return $true }
        Start-Sleep -Seconds 1
    }
    return $false
}

function Stop-PtaHost {
    if (Test-HostRunning) {
        Say "Stopping the running host (the assistant's last state is kept)..."
        Invoke-Pta '--json' 'app' 'quit' | Out-Null
        if (-not (Wait-Host $false)) { Warn 'The host did not stop within a minute.' }
    }
}

# A host started from this session as the user (a GUI-subsystem program: no console window).
function Start-PtaHost {
    if (Test-HostRunning) { return $true }
    Start-Process -FilePath (Join-Path $Bin "$Package.exe") -ArgumentList '--host' -WindowStyle Hidden | Out-Null
    return (Wait-Host $true)
}

function Install-PtaService {
    if (-not (Start-PtaHost)) {
        Warn "The host did not start; run '$Cli status' to see why. It was not registered to start at login."
        return
    }
    $reply = Get-PtaReply 'service' 'install' '--confirmed'
    if (-not $reply -or -not $reply.ok) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "The host was not registered to start at login: $message"
        return
    }
    $name = $reply.data.name
    # Hand the host over to the Task Scheduler, so it runs now as it will after a login.
    Stop-PtaHost
    $started = Invoke-Native schtasks.exe /Run /TN $name
    if ($started.Code -eq 0 -and (Wait-Host $true)) {
        Say "The host runs as a login task ($name)."
    } else {
        Warn 'The Task Scheduler did not start the host now; it starts at your next login.'
        Start-PtaHost | Out-Null
    }
}

# Unattended updating is turned on only when the person passed -UnattendedUpdates.
function Enable-UnattendedUpdates {
    if (-not $WantUnattended) {
        Say "Unattended updates are off. To let this computer install new releases by itself: $Cli update unattended on --confirmed"
        return
    }
    if (-not (Start-PtaHost)) {
        Warn "The host did not start, so unattended updates were not turned on. Later: $Cli update unattended on --confirmed"
        return
    }
    $reply = Get-PtaReply 'update' 'unattended' 'on' '--confirmed'
    if (-not $reply -or -not $reply.ok) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "Unattended updates were not turned on: $message"
        return
    }
    Say 'Unattended updates are on: this computer installs new releases by itself when nothing would be interrupted.'
}

# ---------------------------------------------------------------------------------------------
# PATH for new shells: the user PATH in the registry, keeping its type and every other entry.

function Get-UserPath {
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment')
    try {
        if ($null -eq $key) { return '' }
        return [string] $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    } finally {
        if ($key) { $key.Close() }
    }
}

function Set-UserPath([string] $Value) {
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($key.GetValueNames() -contains 'Path') { $kind = $key.GetValueKind('Path') }
        if ($Value) { $key.SetValue('Path', $Value, $kind) } else { $key.DeleteValue('Path', $false) }
    } finally {
        $key.Close()
    }
    # Tell running programs (Explorer, new terminals) that the environment changed.
    if (-not ('PtaInstall.Environment' -as [type])) {
        Add-Type -Namespace PtaInstall -Name Environment -MemberDefinition @'
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
'@
    }
    $result = [UIntPtr]::Zero
    [PtaInstall.Environment]::SendMessageTimeout([IntPtr] 0xffff, 0x1A, [UIntPtr]::Zero, 'Environment', 2, 5000, [ref] $result) | Out-Null
}

function Test-OurEntry([string] $Entry) { return $Entry.TrimEnd('\') -ieq $Bin }

# The value is kept as it is, empty entries and separators included: adding prepends one entry and
# removing takes out only that entry, so an install and an uninstall leave it byte for byte.
function Add-UserPath {
    $current = Get-UserPath
    if (@($current -split ';' | Where-Object { Test-OurEntry $_ }).Count -gt 0) {
        Say "$Bin is already on your user PATH."
    } else {
        Set-UserPath $(if ($current) { "$Bin;$current" } else { $Bin })
        Say "Added $Bin to your user PATH (new terminals see it)."
    }
    if (-not (($env:Path -split ';') | Where-Object { Test-OurEntry $_ })) { $env:Path = "$Bin;$env:Path" }
}

function Remove-UserPath {
    $current = Get-UserPath
    $parts = @($current -split ';')
    $kept = @($parts | Where-Object { -not (Test-OurEntry $_) })
    if ($kept.Count -ne $parts.Count) { Set-UserPath ($kept -join ';') }
}

# ---------------------------------------------------------------------------------------------
# Coding CLIs: the skill and `pta mcp`, with consent; the exact commands otherwise.

function Register-Integrations {
    $config = Get-PtaReply 'mcp' 'config'
    $command = if ($config -and $config.data -and $config.data.command) { $config.data.command } else { Join-Path $Bin "$Cli.exe" }
    $found = $false
    foreach ($tool in 'claude', 'codex') {
        if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { continue }
        $found = $true
        $skill = Join-Path $env:USERPROFILE ".$tool\skills\$Package"
        $mcp = if ($tool -eq 'claude') { @('mcp', 'add', '--scope', 'user', $Package, '--', $command, 'mcp') } else { @('mcp', 'add', $Package, '--', $command, 'mcp') }
        if (-not $WantIntegrations) {
            Say "  ${tool}: $Cli skill install `"$skill`""
            $shown = foreach ($word in $mcp) { if ($word -match '\s') { '"' + $word + '"' } else { $word } }
            Say ("  ${tool}: $tool " + ($shown -join ' '))
            continue
        }
        if (Test-Path -LiteralPath $skill) {
            Say "${tool}: the skill is already at $skill (updates refresh it while unmodified)."
        } else {
            New-Item -ItemType Directory -Force -Path (Split-Path $skill -Parent) | Out-Null
            $installed = Invoke-Pta 'skill' 'install' $skill
            if ($installed.Code -eq 0) { Say "${tool}: installed the skill at $skill." } else { Warn "${tool}: the skill could not be installed at $skill." }
        }
        if ((Invoke-Native $tool mcp get $Package).Code -eq 0) {
            Say "${tool}: $Package is already registered as an MCP server."
        } else {
            if ((Invoke-Native $tool @mcp).Code -eq 0) { Say "${tool}: registered '$command mcp' as the MCP server $Package." } else { Warn "${tool}: registering the MCP server failed." }
        }
    }
    if (-not $found) {
        Say "No coding CLI (Claude Code, Codex) was found. For any agent, '$Cli mcp config' prints how to add '$Cli mcp'."
    } elseif (-not $WantIntegrations) {
        Say 'Run them yourself, or run this command again with -Integrations.'
    }
}

# ---------------------------------------------------------------------------------------------
# Joining the account.

function Join-Account {
    $settings = Get-PtaReply 'tools' 'call' 'machine_settings'
    $current = if ($settings -and $settings.data -and $settings.data.coordinator) { ([string] $settings.data.coordinator).TrimEnd('/') } else { '' }
    if ($current) {
        if ($current -eq $Origin) { Say "This computer is already connected to $Origin; its machine identity is kept." }
        else { Warn "This computer is connected to $current. Disconnect it there first ('$Cli tools call machine_disconnect'), then run this command again." }
        return
    }
    $reply = Get-PtaReply 'auth' 'machine' 'login' $Origin '--no-browser'
    $url = if ($reply -and $reply.data) { [string] $reply.data.approval_url } else { '' }
    if (-not $url.StartsWith("$Origin/login?enroll=")) {
        $message = if ($reply) { $reply.message } else { 'no answer' }
        Warn "The sign-in could not start: $message"
        Warn "Run '$Cli auth machine login $Origin' when it is fixed."
        return
    }
    Say ''
    Say "Approve this computer in your browser: $url"
    Say "Check that the page shows the code $($reply.data.code), then choose Trust this machine."
    if ($SkipBrowser) { Say 'Open that link on any device where you are signed in to the portal.' }
    else { Start-Process $url }
    Say 'Waiting for the approval (up to ten minutes)...'
    $finished = Get-PtaReply 'auth' 'machine' 'finish' '--wait'
    if ($finished -and $finished.ok -and $finished.data.connected) {
        Say "Connected to $Origin. Choose this computer's allowed directories in the portal (Settings > Machines) before running work on it."
    } else {
        $message = if ($finished) { $finished.message } else { 'no approval' }
        Warn "Not connected yet: $message. After approving, run '$Cli auth machine finish --wait'."
    }
}

# ---------------------------------------------------------------------------------------------
# Install and uninstall.

function Install-Archive {
    $arch = Get-Architecture
    if ($arch -ne 'AMD64') {
        Fail "There is no prebuilt archive for Windows on $arch. Install with Cargo instead: cargo install $Package --locked (needs the Rust toolchain from https://rustup.rs)."
    }
    Assert-NoCargoCopy
    if (-not $Version) {
        if ($TestSource) {
            $found = Get-ChildItem -LiteralPath $TestSource -Filter "$Package-$Target-v*.zip" | Select-Object -First 1
            if ($found -and $found.Name -match '-v([0-9.]+)\.zip$') { $script:Version = $Matches[1] }
        } else {
            $script:Version = Get-LatestVersion
        }
    }
    Test-Version $Version
    $archive = "$Package-$Target-v$Version.zip"
    Say "Installing $Package $Version for $Target into $Root."

    New-Item -ItemType Directory -Force -Path $Root | Out-Null
    $stage = Join-Path $Root (".stage." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    New-Item -ItemType Directory -Path $stage | Out-Null
    try {
        $zip = Join-Path $stage $archive
        $sums = "$zip.sha256"
        if (-not (Get-Asset $archive $zip) -or -not (Get-Asset "$archive.sha256" $sums)) {
            Fail "Release $Version has no archive for $Target, or it could not be downloaded. Nothing was changed. Check the version and the network connection and try again."
        }
        $line = (Get-Content -LiteralPath $sums -TotalCount 1).Trim() -split '\s+'
        $expected = $line[0].ToLowerInvariant()
        $named = if ($line.Count -gt 1) { $line[1].TrimStart('*') } else { '' }
        $digest = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($named -ne $archive -or $digest -ne $expected) {
            Fail "The download does not match the release's published SHA-256. Nothing was changed. Try again; if it keeps failing, report it at https://github.com/$Repository/issues."
        }
        Say 'The archive matches its published SHA-256.'
        if ($TestSource) {
            Say 'Build provenance not checked (test source).'
        } elseif (Test-GhSignedIn) {
            if ((Invoke-Native gh attestation verify $zip --repo $Repository).Code -ne 0) { Fail "GitHub could not verify the archive's build provenance. Nothing was changed. Run 'gh attestation verify' on the archive to see why." }
            Say 'Build provenance verified with gh attestation verify.'
        } else {
            Say 'Build provenance not checked: install the GitHub CLI (gh) and sign in to verify it. The SHA-256 matched.'
        }

        $unpacked = Join-Path $stage 'unpacked'
        $newBin = Join-Path $stage 'bin'
        New-Item -ItemType Directory -Path $newBin | Out-Null
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        try { [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $unpacked) } catch { Fail 'The archive could not be unpacked. Nothing was changed.' }
        foreach ($name in $Package, $Cli) {
            $file = Join-Path $unpacked "$Package-$Target-v$Version\$name.exe"
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { Fail "The archive does not contain $name.exe. Nothing was changed." }
            Copy-Item -LiteralPath $file -Destination (Join-Path $newBin "$name.exe")
        }
        $reported = (Invoke-Native (Join-Path $newBin "$Cli.exe") --version --json).Text
        if ($reported -notmatch [regex]::Escape("`"cli_version`":`"$Version`"")) {
            Fail 'The unpacked pta does not run on this computer or reports another version. Nothing was changed.'
        }

        if (Test-Path -LiteralPath (Get-ProfileDir)) { Say "Using the existing profile at $(Get-ProfileDir); it is kept as it is." }
        # Only now does anything installed change: stop the old host, swap the bin directory. A
        # running executable cannot be replaced on Windows, so the host is stopped first.
        Stop-PtaHost
        $previous = Join-Path $stage 'previous'
        if (Test-Path -LiteralPath $Bin) {
            try { Move-Item -LiteralPath $Bin -Destination $previous } catch { Fail 'The previous installation could not be replaced (is a pta command still running?). Nothing was changed.' }
        }
        try {
            Move-Item -LiteralPath $newBin -Destination $Bin
        } catch {
            if (Test-Path -LiteralPath $previous) { Move-Item -LiteralPath $previous -Destination $Bin }
            Fail 'The new binaries could not be moved into place; the previous installation was restored.'
        }
        $recordJson = [ordered] @{
            schema = 1; kind = 'archive'; package = $Package; version = $Version; target = $Target
            bins = @($Package, $Cli); repository = "https://github.com/$Repository"; archive = $archive
            sha256 = $digest; installed_at = [DateTime]::UtcNow.ToString('yyyy-MM-ddTHH:mm:ssZ')
        } | ConvertTo-Json
        $recordFile = Join-Path $stage $Record
        [System.IO.File]::WriteAllText($recordFile, $recordJson, (New-Object System.Text.UTF8Encoding $false))
        Move-Item -LiteralPath $recordFile -Destination (Join-Path $Root $Record) -Force
    } finally {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    }
    Say "Installed $((Invoke-Pta '--version').Text) in $Bin."
    Clear-StaleBinstallRecord

    Add-UserPath
    if ($SkipService) { Say 'Not registered to start at login (-NoService).' } else { Install-PtaService }
    Enable-UnattendedUpdates
    if ($Origin) { Join-Account }
    Say ''
    Say 'Skill and MCP for coding CLIs on this computer:'
    Register-Integrations
    Say ''
    Say "Open a new terminal to use '$Cli'. To remove the binaries later, run this script with -Uninstall (the profile is kept)."
}

function Uninstall-Archive {
    $recordFile = Join-Path $Root $Record
    if (-not (Test-Path -LiteralPath $recordFile) -or ((Get-Content -LiteralPath $recordFile -Raw | ConvertFrom-Json).kind -ne 'archive')) {
        Fail "No installation by this script was found in $Root."
    }
    if (Test-Path -LiteralPath (Join-Path $Bin "$Cli.exe")) {
        if (Start-PtaHost) {
            $reply = Get-PtaReply 'service' 'uninstall' '--confirmed'
            if (-not $reply -or -not $reply.ok) { Warn "The login task could not be removed; remove it with '$Cli service uninstall --confirmed'." }
        }
        Stop-PtaHost
    }
    Remove-UserPath
    Remove-Item -LiteralPath $Bin -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $recordFile -Force
    if (-not (Get-ChildItem -LiteralPath $Root -Force)) { Remove-Item -LiteralPath $Root -Force }
    Say "Removed $Package from $Root. The profile at $(Get-ProfileDir) was kept."
    Say "A skill copy or MCP registration made for a coding CLI stays; remove it there (for example 'claude mcp remove $Package')."
}

# ---------------------------------------------------------------------------------------------

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    if ($TestSource) {
        if ($env:PTA_INSTALL_TEST -ne '1') { Fail '-TestSource is only for the installer''s own tests.' }
        if (-not (Test-Path -LiteralPath $TestSource -PathType Container)) { Fail 'The test source must be a local directory.' }
        $TestSource = (Resolve-Path -LiteralPath $TestSource).Path
        Warn "TEST MODE: installing from $TestSource instead of the release."
    }
    if ($Origin) { $Origin = Test-Origin $Origin }
    if ($Version) { Test-Version $Version }
    if (-not $Root) { $Root = if ($env:PTA_INSTALL_ROOT) { $env:PTA_INSTALL_ROOT } else { Join-Path $env:LOCALAPPDATA "Programs\$Package" } }
    $Root = [System.IO.Path]::GetFullPath($Root).TrimEnd('\')
    $Bin = Join-Path $Root 'bin'
    if ($Uninstall) { Uninstall-Archive } else { Install-Archive }
} catch {
    Write-Host $_.Exception.Message -ForegroundColor Red
    exit 1
}
exit 0
