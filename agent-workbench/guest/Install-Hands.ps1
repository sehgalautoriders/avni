<#
.SYNOPSIS
    Installs the AVNI "hands" service (screen, mouse, keyboard) INSIDE the Windows 11 guest VM.

.DESCRIPTION
    Run this in the GUEST, as the logged-in user, from the folder that contains avni_hands.py.
    Administrator rights are needed ONLY for the firewall rule; without them the script
    skips that step and prints the exact command to run later from an elevated prompt.

    Steps:
      1. Refuses to run on a machine that does not look like a Hyper-V guest (override: -AllowHost).
      2. Checks Python (py -3 --version, then python --version). If missing it prints and runs
         the exact command:  winget install -e --id Python.Python.3.12 --scope user
      3. Copies the guest folder to %LOCALAPPDATA%\Avni\workbench\hands\ and runs pip install -r requirements.txt.
      4. Creates the inbound firewall rule "AVNI Hands 7001" (TCP 7001) that accepts ONLY the
         default gateway address, which on the Hyper-V Default Switch is the host itself.
         The service also refuses any other client address itself (403).
      5. Registers Scheduled Task \AVNI\Hands that runs pythonw.exe avni_hands.py
         --archive-dir %LOCALAPPDATA%\Avni\workbench\shots at logon of the current user.
      6. Starts the task, waits up to 20 s, then MEASURES the service with
         Invoke-WebRequest http://127.0.0.1:7001/health and prints the status codes.
         Without a token the expected status is 401 (proves the server is up AND auth is enforced).
      7. Prints the token file path (never the token).

    It never writes to Desktop or Documents.

.PARAMETER InstallDir
    Where the service files are copied. Default: %LOCALAPPDATA%\Avni\workbench\hands

.PARAMETER ArchiveDir
    Where every screenshot is archived as PNG. Default: %LOCALAPPDATA%\Avni\workbench\shots

.PARAMETER EnableShell
    Start the service with --enable-shell so POST /shell works. Off by default: the
    agent then has mouse, keyboard and screen only.

.PARAMETER AllowHost
    Skip the "is this a Hyper-V guest?" safety check. Do NOT use on the physical host:
    the service would take over the owner's real mouse and keyboard.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\Install-Hands.ps1

.NOTES
    The service must run in the INTERACTIVE desktop session of the logged-in user, because
    SendInput only reaches the desktop of the session it runs in. That is why this is a
    logon-triggered task with LogonType Interactive and not a Windows service. The guest
    therefore needs automatic sign-in (or someone signed in) for the agent to have hands.
#>
[CmdletBinding()]
param(
    [string]$InstallDir = (Join-Path $env:LOCALAPPDATA 'Avni\workbench\hands'),
    [string]$ArchiveDir = (Join-Path $env:LOCALAPPDATA 'Avni\workbench\shots'),
    [switch]$EnableShell,
    [switch]$AllowHost
)

$ErrorActionPreference = 'Stop'
$Port        = 7001
$RuleName    = 'AVNI Hands 7001'
$TaskPath    = '\AVNI\'
$TaskName    = 'Hands'
$BaseDir     = Join-Path $env:LOCALAPPDATA 'Avni\workbench'
$TokenFile   = Join-Path $BaseDir 'hands.token'
$SourceDir   = $PSScriptRoot
$WingetCmd   = 'winget install -e --id Python.Python.3.12 --scope user'

function Write-Step([string]$Text) { Write-Host "[hands] $Text" -ForegroundColor Cyan }

function Test-IsAdmin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    return ([Security.Principal.WindowsPrincipal]$id).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

function Test-HyperVGuest {
    try {
        $model = (Get-CimInstance Win32_ComputerSystem).Model
        return ($model -like '*Virtual Machine*'), $model
    } catch {
        return $false, '(could not read Win32_ComputerSystem)'
    }
}

function Update-SessionPath {
    # winget/user installs change PATH; refresh this session so python/py are found without a new shell.
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $user    = [Environment]::GetEnvironmentVariable('Path', 'User')
    $env:Path = "$machine;$user"
}

function Find-Python {
    # Returns the full path of a working python.exe, or $null. Uses --version to reject the Microsoft Store stub.
    $candidates = @()
    foreach ($probe in @(@('py', '-3'), @('python'))) {
        $exe = Get-Command $probe[0] -ErrorAction SilentlyContinue
        if (-not $exe) { continue }
        $extra = @($probe | Select-Object -Skip 1)
        $versionOk = $false
        try {
            $out = & $exe.Source @extra --version 2>&1
            $versionOk = ($LASTEXITCODE -eq 0) -and ("$out" -match 'Python 3\.(\d+)') -and ([int]$Matches[1] -ge 11)
        } catch { $versionOk = $false }
        if ($versionOk) {
            $path = & $exe.Source @extra -c 'import sys; print(sys.executable)' 2>$null
            if ($LASTEXITCODE -eq 0 -and $path -and (Test-Path "$path".Trim())) { return "$path".Trim() }
        }
    }
    $local = Join-Path $env:LOCALAPPDATA 'Programs\Python'
    if (Test-Path $local) {
        $found = Get-ChildItem $local -Directory -Filter 'Python3*' | Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName 'python.exe' } | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($found) { return $found }
    }
    return $null
}

function Get-HttpStatus([string]$Url, [hashtable]$Headers = @{}) {
    # Returns the HTTP status code (also for 4xx/5xx) or $null when nothing is listening.
    try {
        $r = Invoke-WebRequest -Uri $Url -Headers $Headers -UseBasicParsing -TimeoutSec 3
        return [int]$r.StatusCode
    } catch {
        $resp = $_.Exception.Response
        if ($resp) { return [int]$resp.StatusCode }
        return $null
    }
}

# --- 1. guest-only safety check -------------------------------------------------
Write-Step 'checking that this machine is a Hyper-V guest'
$isGuest, $model = Test-HyperVGuest
if (-not $isGuest -and -not $AllowHost) {
    throw "This does not look like a Hyper-V guest (model: $model). Refusing to install: the service would control the real mouse and keyboard. Run it inside the VM (or pass -AllowHost if you really mean it)."
}
Write-Host "        machine model: $model"

# --- 2. python -----------------------------------------------------------------
Write-Step 'checking Python (py -3 --version / python --version)'
$python = Find-Python
if (-not $python) {
    Write-Host "        Python 3.11+ not found. Running:  $WingetCmd" -ForegroundColor Yellow
    Invoke-Expression $WingetCmd
    if ($LASTEXITCODE -ne 0) { throw "winget failed (exit $LASTEXITCODE). Install Python 3.12 manually and re-run." }
    Update-SessionPath
    $python = Find-Python
    if (-not $python) { throw 'Python was installed but could not be located; open a new PowerShell window and re-run this script.' }
}
$pythonw = Join-Path (Split-Path $python -Parent) 'pythonw.exe'
if (-not (Test-Path $pythonw)) { throw "pythonw.exe not found next to $python" }
Write-Host "        python : $python"
Write-Host "        pythonw: $pythonw"

# --- 3. copy files + pip -------------------------------------------------------
Write-Step "copying service to $InstallDir"
New-Item -ItemType Directory -Force -Path $InstallDir, $ArchiveDir, $BaseDir | Out-Null
foreach ($name in 'avni_hands.py', 'keymap.py', 'requirements.txt', 'Install-Hands.ps1') {
    $src = Join-Path $SourceDir $name
    if (-not (Test-Path $src)) { throw "missing $src (run this script from the guest folder)" }
    if ($src -ne (Join-Path $InstallDir $name)) { Copy-Item -LiteralPath $src -Destination $InstallDir -Force }
}
Write-Step 'pip install -r requirements.txt'
& $python -m pip install --disable-pip-version-check -r (Join-Path $InstallDir 'requirements.txt')
if ($LASTEXITCODE -ne 0) { throw "pip install failed (exit $LASTEXITCODE)" }

# --- 4. firewall (admin only) --------------------------------------------------
$gateway = $null
try {
    $gateway = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction Stop | Sort-Object RouteMetric, InterfaceMetric | Select-Object -First 1).NextHop
} catch { $gateway = $null }
if (-not $gateway) { throw 'Could not read the default gateway (Get-NetRoute 0.0.0.0/0). The firewall rule must be limited to the host address; refusing to open the port to everyone.' }
Write-Step "firewall rule '$RuleName' (TCP $Port inbound, ONLY from the host at $gateway)"
$ruleCmd = "New-NetFirewallRule -DisplayName '$RuleName' -Direction Inbound -Protocol TCP -LocalPort $Port -RemoteAddress $gateway -Action Allow -Profile Any"
if (Test-IsAdmin) {
    Get-NetFirewallRule -DisplayName $RuleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName $RuleName -Direction Inbound -Protocol TCP -LocalPort $Port `
        -RemoteAddress $gateway -Action Allow -Profile Any | Out-Null
    Write-Host "        rule created (remote address limited to $gateway)"
} else {
    Write-Warning "Not elevated: firewall rule skipped. From an ADMIN PowerShell run:`n        $ruleCmd"
}
try {
    $profiles = (Get-NetConnectionProfile | ForEach-Object { "$($_.Name)=$($_.NetworkCategory)" }) -join ', '
    Write-Host "        network profile(s): $profiles"
    if ($profiles -notmatch 'Private|DomainAuthenticated') {
        Write-Warning 'No Private/Domain network: the host driver will be blocked by the firewall. If this is the Hyper-V internal switch, set it to Private (Set-NetConnectionProfile -NetworkCategory Private) as admin.'
    }
} catch { }

# --- 5. scheduled task ---------------------------------------------------------
Write-Step "registering scheduled task $TaskPath$TaskName (at logon of $env:USERDOMAIN\$env:USERNAME)"
$script = Join-Path $InstallDir 'avni_hands.py'
$serviceArgs = '"{0}" --archive-dir "{1}"' -f $script, $ArchiveDir
if ($EnableShell) { $serviceArgs += ' --enable-shell' }
$action = New-ScheduledTaskAction -Execute $pythonw -Argument $serviceArgs -WorkingDirectory $InstallDir
$user   = "$env:USERDOMAIN\$env:USERNAME"
$trigger   = New-ScheduledTaskTrigger -AtLogOn -User $user
# Interactive logon type: the task runs inside the user's desktop session, which SendInput requires.
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings  = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1)
$existing = Get-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -ErrorAction SilentlyContinue
if ($existing) { Stop-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -ErrorAction SilentlyContinue }
Register-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Force `
    -Description 'AVNI Agent Workbench guest hands service (screen, mouse, keyboard for the VM only)' | Out-Null

# --- 6. start + measure --------------------------------------------------------
Write-Step 'starting the task'
Start-ScheduledTask -TaskPath $TaskPath -TaskName $TaskName
$healthUrl = "http://127.0.0.1:$Port/health"
$noToken = $null
$deadline = (Get-Date).AddSeconds(20)
while ((Get-Date) -lt $deadline -and $null -eq $noToken) {
    Start-Sleep -Milliseconds 500
    $noToken = Get-HttpStatus $healthUrl
}
if ($null -eq $noToken) {
    Write-Host "MEASURED: no HTTP answer from $healthUrl within 20 s" -ForegroundColor Red
    Write-Host "        look at $BaseDir\hands.log and run 'Get-ScheduledTaskInfo -TaskPath $TaskPath -TaskName $TaskName'"
    exit 1
}
Write-Host "MEASURED: GET /health without token -> HTTP $noToken (expected 401)" -ForegroundColor Green
if ($noToken -ne 401) { Write-Warning 'Expected 401: authentication does not appear to be enforced!' }
if (Test-Path $TokenFile) {
    $withToken = Get-HttpStatus $healthUrl @{ 'X-Avni-Token' = (Get-Content -LiteralPath $TokenFile -Raw).Trim() }
    Write-Host "MEASURED: GET /health with token    -> HTTP $withToken (expected 200)"
}

# --- 7. summary ----------------------------------------------------------------
Write-Host ''
Write-Host "Token file : $TokenFile   (give this file's content to the host driver; it is never printed)"
Write-Host "Service    : $script"
Write-Host "Log        : $BaseDir\hands.log"
Write-Host "Screenshots: $ArchiveDir"
Write-Host "Pause      : create the file $BaseDir\PAUSE to block all input; delete it to resume"
Write-Host ("Clients    : localhost and the host at " + $gateway + " only (service refuses others with 403)")
Write-Host ("Shell      : " + $(if ($EnableShell) { 'ENABLED (--enable-shell)' } else { 'disabled; re-run with -EnableShell to allow POST /shell' }))
