#Requires -Version 5.1
<#
.SYNOPSIS
Day-to-day operations for the AVNI Agent Workbench Hyper-V VM (status, start, stop, checkpoints, IP, viewer, file copy).

.DESCRIPTION
One script, one -Action. Every action prints what it measured (value, method,
time) and appends one JSON line to %LOCALAPPDATA%\Avni\reports\workbench-ops.log
with the time, action and result.

Actions:
  Status       State, uptime, CPU %, memory assigned/demand, heartbeat, IP addresses,
               latest checkpoint, AutomaticStartAction. Read-only.
  Start        Starts the VM (never if it is already running), then waits up to 120 s
               (polling every 5 s) for the guest heartbeat.
  Shutdown     Graceful guest shutdown (needs integration services). Waits up to 180 s.
  Stop         Power-off, like pulling the plug. Only runs if you also pass -Force.
  Checkpoint   Creates a checkpoint named "<dd-MM-yyyy HHmm> <Label>" and lists checkpoints.
  Checkpoints  Lists checkpoints, newest first, with creation time. Read-only.
  Restore      Needs -CheckpointName (exact). Without -Force it only says what it WOULD do.
  Ip           First IPv4 address of the VM and a TCP test of port 7001 (the guest "hands" service).
  Connect      Opens the vmconnect.exe viewer window for the VM.
  Copy         Copies one file from the host into the guest (Copy-VMFile, Guest Service Interface).

What it never does:
  - It never starts the VM except for -Action Start, which you type yourself.
  - It never uses a forced stop unless you pass -Force to -Action Stop.
  - It never restores a checkpoint without -Force.
  - It never deletes a VM, a disk or a checkpoint.
  - It never changes the VM's settings (no Set-VM, no memory or disk changes).
  - It never writes to Desktop, Documents or project folders; the only thing it
    writes is the ops log under %LOCALAPPDATA%\Avni\reports\.
  - It never touches AutoHotkey or any other program on the host.
  - It contains no secrets and makes no internet connections (the port test only
    talks to the VM).

Needs an elevated PowerShell (or membership of the "Hyper-V Administrators" group).

Exit code: 0 = done (or nothing to do), 1 = refused or failed.

.PARAMETER Action
One of Status, Start, Stop, Shutdown, Checkpoint, Checkpoints, Restore, Ip, Connect, Copy.

.PARAMETER Name
VM name. Default 'AVNI Agent Workbench 01'.

.PARAMETER Label
Optional text added to the checkpoint name (-Action Checkpoint).

.PARAMETER CheckpointName
Exact checkpoint name to restore (-Action Restore). Get it from -Action Checkpoints.

.PARAMETER Source
Host file to copy (-Action Copy).

.PARAMETER Destination
Full file path inside the guest, including the file name (-Action Copy).

.PARAMETER Port
TCP port tested by -Action Ip. Default 7001.

.PARAMETER Force
Explicit permission for the dangerous actions: power-off (-Action Stop) and
restoring a checkpoint (-Action Restore).

.PARAMETER Overwrite
Allow -Action Copy to replace a file that already exists in the guest.

.EXAMPLE
.\Workbench.ps1 -Action Status

.EXAMPLE
.\Workbench.ps1 -Action Checkpoint -Label "clean install"

.EXAMPLE
.\Workbench.ps1 -Action Restore -CheckpointName "10-10-2026 1430 clean install" -Force

.EXAMPLE
.\Workbench.ps1 -Action Copy -Source .\hands.zip -Destination C:\Avni\hands.zip
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet('Status', 'Start', 'Stop', 'Shutdown', 'Checkpoint', 'Checkpoints', 'Restore', 'Ip', 'Connect', 'Copy')]
    [string]$Action,

    [ValidatePattern('^[A-Za-z0-9]([A-Za-z0-9 ._-]{0,62}[A-Za-z0-9])?$')]
    [string]$Name = 'AVNI Agent Workbench 01',

    [string]$Label,
    [string]$CheckpointName,
    [string]$Source,
    [string]$Destination,

    [ValidateRange(1, 65535)]
    [int]$Port = 7001,

    [switch]$Force,
    [switch]$Overwrite
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Script-wide state and constants
# ---------------------------------------------------------------------------
$script:ToolVersion       = '1.0.0'
$script:Culture           = [System.Globalization.CultureInfo]::InvariantCulture
$script:ExitCode          = 0
$script:OpsResult         = 'unknown'
$script:OpsDetail         = ''
$script:StartTimeoutSec   = 120
$script:ShutdownTimeoutSec = 180
$script:PollSec           = 5
$script:HeartbeatId       = '84EAAE65-2F2E-45F5-9BB5-0E857DC8EB47'
$script:GuestServiceId    = '6C09BB55-D683-4DA0-8931-C9BF705F6480'
$script:ShutdownId        = '9F8233AC-BE49-4C79-8EE3-E7E1985B2077'

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
function Get-Stamp {
    return (Get-Date).ToString('yyyy-MM-dd HH:mm:ss', $script:Culture)
}

function Get-PropValue {
    param($Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    if ($Object.PSObject.Properties.Match($Name).Count -gt 0) { return $Object.$Name }
    return $null
}

function Format-GB {
    param($Bytes)
    if ($null -eq $Bytes) { return 'n/a' }
    return (([double]$Bytes) / 1GB).ToString('0.00', $script:Culture)
}

function Format-Duration {
    param($Span)
    if ($null -eq $Span) { return 'n/a' }
    $ts = [TimeSpan]$Span
    return ('{0}d {1:00}h {2:00}m {3:00}s' -f $ts.Days, $ts.Hours, $ts.Minutes, $ts.Seconds)
}

function Limit-Text {
    param([string]$Text, [int]$Max = 200)
    if ($null -eq $Text) { return '' }
    $flat = ($Text -replace '\s+', ' ').Trim()
    if ($flat.Length -le $Max) { return $flat }
    return $flat.Substring(0, $Max - 3) + '...'
}

function Test-CanManageHyperV {
    # True for an elevated administrator or a member of "Hyper-V Administrators".
    try {
        $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
        if ($principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) { return $true }
        $sid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-578')
        return $principal.IsInRole($sid)
    }
    catch {
        return $false
    }
}

function Write-Tagged {
    param([string]$Tag, [string]$Text, [string]$Color = '')
    $line = ('{0,-14} {1}' -f $Tag, $Text)
    if ([string]::IsNullOrEmpty($Color)) { Write-Host $line } else { Write-Host $line -ForegroundColor $Color }
}

function Write-Detail {
    param([string]$Text)
    Write-Host ('{0,-14}   {1}' -f '', $Text) -ForegroundColor DarkGray
}

function New-MeasureResult {
    param($Value, [string]$Display = '')
    $result = [pscustomobject]@{ Value = $Value; Display = $Display }
    $result.PSObject.TypeNames.Insert(0, 'Avni.MeasureResult')
    return $result
}

function Invoke-Measure {
    # Runs a read-only probe, prints what was measured + value + method + time, returns a record.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$What,
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][scriptblock]$Probe,
        [string]$Color = 'Cyan'
    )
    $mxAt = Get-Stamp
    $mxStatus = 'MEASURED'
    $mxValue = $null
    $mxDisplay = ''
    try {
        $mxOut = @(& $Probe)
        $mxResult = $null
        foreach ($mxItem in $mxOut) {
            if ($null -ne $mxItem -and ($mxItem.PSObject.TypeNames -contains 'Avni.MeasureResult')) { $mxResult = $mxItem }
        }
        if ($null -eq $mxResult) { throw 'the probe returned no value' }
        $mxValue = $mxResult.Value
        $mxDisplay = [string]$mxResult.Display
        if ([string]::IsNullOrEmpty($mxDisplay)) { $mxDisplay = [string]$mxValue }
    }
    catch {
        $mxStatus = 'NOT MEASURED'
        $mxDisplay = 'NOT MEASURED (' + (Limit-Text -Text $_.Exception.Message -Max 200) + ')'
    }
    $record = [pscustomobject]([ordered]@{
        What = $What; Status = $mxStatus; Value = $mxValue; Display = $mxDisplay; Method = $Method; MeasuredAt = $mxAt
    })
    if ($mxStatus -eq 'MEASURED') { Write-Tagged -Tag '[MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color $Color }
    else { Write-Tagged -Tag '[NOT MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color 'Yellow' }
    Write-Detail ('method: ' + $Method + ' | at ' + $mxAt)
    return $record
}

function Set-Outcome {
    param([string]$Result, [string]$Detail = '', [int]$Code = 0)
    $script:OpsResult = $Result
    $script:OpsDetail = $Detail
    $script:ExitCode = $Code
}

function Write-OpsLog {
    # One JSON line per run. Failure to log is reported but never changes the action's result.
    try {
        if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
        $dir = Join-Path -Path $env:LOCALAPPDATA -ChildPath 'Avni\reports'
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $entry = [ordered]@{
            timestamp = (Get-Date).ToString('yyyy-MM-ddTHH:mm:sszzz', $script:Culture)
            action    = $Action
            vm        = $Name
            result    = $script:OpsResult
            detail    = (Limit-Text -Text $script:OpsDetail -Max 500)
            exitCode  = $script:ExitCode
            tool      = ('Workbench.ps1 ' + $script:ToolVersion)
        }
        $line = ConvertTo-Json -InputObject $entry -Compress
        $path = Join-Path -Path $dir -ChildPath 'workbench-ops.log'
        [System.IO.File]::AppendAllText($path, $line + [System.Environment]::NewLine, (New-Object System.Text.UTF8Encoding($false)))
    }
    catch {
        Write-Host ('WARNING: could not append to workbench-ops.log: ' + $_.Exception.Message) -ForegroundColor Yellow
    }
}

# ---------------------------------------------------------------------------
# Hyper-V lookups
# ---------------------------------------------------------------------------
function Get-TargetVm {
    $found = @(Get-VM -Name $Name -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { throw ("no VM named '" + $Name + "' (Get-VM found none)") }
    return $found[0]
}

function Get-IntegrationServiceByKey {
    # Finds an integration service by English name first, then by its well-known id (other languages).
    param([string]$EnglishName, [string]$IdSuffix)
    $services = @(Get-VMIntegrationService -VMName $Name)
    foreach ($svc in $services) {
        if ($svc.Name -eq $EnglishName) { return $svc }
    }
    foreach ($svc in $services) {
        $id = [string](Get-PropValue -Object $svc -Name 'Id')
        if ($id -match [regex]::Escape($IdSuffix)) { return $svc }
    }
    return $null
}

function Get-VmIpAddresses {
    $all = New-Object System.Collections.Generic.List[string]
    foreach ($adapter in @(Get-VMNetworkAdapter -VMName $Name)) {
        foreach ($ip in @($adapter.IPAddresses)) {
            if ($ip) { $all.Add([string]$ip) }
        }
    }
    return $all.ToArray()
}

function Get-FirstUsableIPv4 {
    param([string[]]$Addresses)
    foreach ($a in $Addresses) {
        if ($a -match '^\d{1,3}(\.\d{1,3}){3}$' -and -not $a.StartsWith('169.254.')) { return $a }
    }
    return $null
}

function Get-SortedSnapshots {
    return @(Get-VMSnapshot -VMName $Name -ErrorAction SilentlyContinue | Sort-Object -Property CreationTime -Descending)
}

function Format-SnapshotLine {
    param($Snapshot)
    $parent = [string](Get-PropValue -Object $Snapshot -Name 'ParentSnapshotName')
    if (-not $parent) { $parent = '(none)' }
    return ('{0} | created {1} | parent {2}' -f $Snapshot.Name, $Snapshot.CreationTime.ToString('dd-MM-yyyy HH:mm:ss', $script:Culture), $parent)
}

function Show-CheckpointList {
    $null = Invoke-Measure -What ("Checkpoints of '" + $Name + "' (newest first)") -Method 'Get-VMSnapshot (Name, CreationTime, ParentSnapshotName)' -Probe {
        $snaps = @(Get-SortedSnapshots)
        if ($snaps.Count -eq 0) { return (New-MeasureResult -Value @() -Display 'none (Get-VMSnapshot returned 0 objects)') }
        $lines = @(foreach ($s in $snaps) { Format-SnapshotLine -Snapshot $s })
        New-MeasureResult -Value $lines -Display ($lines -join "`n")
    }
}

function Wait-VmState {
    # Polls Get-VM until State equals $Wanted or the timeout passes. Returns the last state seen.
    param([string]$Wanted, [int]$TimeoutSec)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    $state = ''
    while ($true) {
        $state = [string](Get-TargetVm).State
        if ($state -eq $Wanted) { break }
        if ((Get-Date) -ge $deadline) { break }
        Start-Sleep -Seconds $script:PollSec
    }
    return $state
}

# ---------------------------------------------------------------------------
# Actions
# ---------------------------------------------------------------------------
function Invoke-StatusAction {
    $null = Invoke-Measure -What 'State' -Method 'Get-VM (State)' -Probe {
        $s = [string](Get-TargetVm).State
        New-MeasureResult -Value $s -Display $s
    }
    $null = Invoke-Measure -What 'Uptime' -Method 'Get-VM (Uptime)' -Probe {
        $t = (Get-TargetVm).Uptime
        New-MeasureResult -Value ([string]$t) -Display (Format-Duration -Span $t)
    }
    $null = Invoke-Measure -What 'CPU usage' -Method 'Get-VM (CPUUsage, percent of the VM processors)' -Probe {
        $c = (Get-TargetVm).CPUUsage
        New-MeasureResult -Value $c -Display ([string]$c + ' %')
    }
    $null = Invoke-Measure -What 'Memory assigned / demand' -Method 'Get-VM (MemoryAssigned, MemoryDemand)' -Probe {
        $vm = Get-TargetVm
        $info = [pscustomobject]([ordered]@{ AssignedGB = (Format-GB -Bytes $vm.MemoryAssigned); DemandGB = (Format-GB -Bytes $vm.MemoryDemand) })
        New-MeasureResult -Value $info -Display ("assigned {0} GB / demand {1} GB" -f $info.AssignedGB, $info.DemandGB)
    }
    $null = Invoke-Measure -What 'Heartbeat (VM property)' -Method 'Get-VM (Heartbeat)' -Probe {
        $hb = Get-PropValue -Object (Get-TargetVm) -Name 'Heartbeat'
        if ($null -eq $hb) { throw 'Hyper-V reported no Heartbeat value' }
        New-MeasureResult -Value ([string]$hb) -Display ([string]$hb)
    }
    $null = Invoke-Measure -What 'Heartbeat integration service' -Method 'Get-VMIntegrationService (Heartbeat: Enabled, PrimaryStatusDescription)' -Probe {
        $svc = Get-IntegrationServiceByKey -EnglishName 'Heartbeat' -IdSuffix $script:HeartbeatId
        if ($null -eq $svc) { throw 'Heartbeat integration service not found' }
        $text = "enabled {0}, status {1}" -f $svc.Enabled, (Get-PropValue -Object $svc -Name 'PrimaryStatusDescription')
        New-MeasureResult -Value $text -Display $text
    }
    $null = Invoke-Measure -What 'Guest Service Interface (file copy)' -Method 'Get-VMIntegrationService (Guest Service Interface: Enabled, PrimaryStatusDescription)' -Probe {
        $svc = Get-IntegrationServiceByKey -EnglishName 'Guest Service Interface' -IdSuffix $script:GuestServiceId
        if ($null -eq $svc) { throw 'Guest Service Interface not found' }
        $text = "enabled {0}, status {1}" -f $svc.Enabled, (Get-PropValue -Object $svc -Name 'PrimaryStatusDescription')
        New-MeasureResult -Value $text -Display $text
    }
    $null = Invoke-Measure -What 'IP addresses' -Method '(Get-VMNetworkAdapter).IPAddresses' -Probe {
        $ips = @(Get-VmIpAddresses)
        if ($ips.Count -eq 0) { return (New-MeasureResult -Value @() -Display 'none reported (VM off, or guest integration services not running yet)') }
        New-MeasureResult -Value $ips -Display ($ips -join ', ')
    }
    $null = Invoke-Measure -What 'Latest checkpoint' -Method 'Get-VMSnapshot (newest by CreationTime)' -Probe {
        $snaps = @(Get-SortedSnapshots)
        if ($snaps.Count -eq 0) { return (New-MeasureResult -Value $null -Display 'none (Get-VMSnapshot returned 0 objects)') }
        $line = Format-SnapshotLine -Snapshot $snaps[0]
        New-MeasureResult -Value $line -Display $line
    }
    $auto = Invoke-Measure -What 'AutomaticStartAction (must be Nothing)' -Method 'Get-VM (AutomaticStartAction)' -Probe {
        $a = [string](Get-TargetVm).AutomaticStartAction
        New-MeasureResult -Value $a -Display $a
    }
    if ($auto.Status -eq 'MEASURED' -and $auto.Value -ne 'Nothing') {
        Write-Tagged -Tag '[WARNING]' -Text ("AutomaticStartAction is '" + $auto.Value + "', not Nothing: the VM could start when the host boots. Fix with: Set-VM -Name '" + $Name + "' -AutomaticStartAction Nothing") -Color 'Red'
    }
    Set-Outcome -Result 'ok' -Detail 'status read'
}

function Invoke-StartAction {
    $vm = Get-TargetVm
    $state = [string]$vm.State
    if ($state -eq 'Running') {
        Write-Host ("The VM is already running (Get-VM State = Running). Nothing was started.")
        Set-Outcome -Result 'noop-already-running' -Detail 'VM already Running'
        return
    }
    if ($state -notin @('Off', 'Saved')) {
        Write-Host ("Refusing to start: the VM state is '" + $state + "' (expected Off or Saved).") -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail ("state is " + $state) -Code 1
        return
    }
    Write-Host ("Starting '" + $Name + "' (it was " + $state + ") ...")
    Start-VM -Name $Name
    $started = Get-Date
    $deadline = $started.AddSeconds($script:StartTimeoutSec)
    $okStreak = 0
    $final = 'timeout'
    $lastHb = ''
    while ($true) {
        $vm = Get-TargetVm
        $hb = [string](Get-PropValue -Object $vm -Name 'Heartbeat')
        $lastHb = $hb
        $elapsed = [int]((Get-Date) - $started).TotalSeconds
        Write-Tagged -Tag '[MEASURED]' -Text ("t+{0}s State={1} Heartbeat={2}" -f $elapsed, $vm.State, $hb) -Color 'Cyan'
        Write-Detail ('method: Get-VM (State, Heartbeat) | at ' + (Get-Stamp))
        if ($hb -eq 'OkApplicationsHealthy') { $final = 'healthy'; break }
        if ($hb -like 'Ok*') { $okStreak++ } else { $okStreak = 0 }
        if ($okStreak -ge 3) { $final = 'ok-apps-unknown'; break }
        if ((Get-Date) -ge $deadline) { break }
        Start-Sleep -Seconds $script:PollSec
    }
    switch ($final) {
        'healthy' {
            Write-Host 'Heartbeat reports OkApplicationsHealthy.'
            Set-Outcome -Result 'started-heartbeat-healthy' -Detail 'OkApplicationsHealthy'
        }
        'ok-apps-unknown' {
            Write-Host ("Heartbeat is present but reports '" + $lastHb + "' (guest alive, application health not reported).") -ForegroundColor Yellow
            Set-Outcome -Result 'started-heartbeat-ok-apps-unknown' -Detail $lastHb
        }
        default {
            Write-Host ("TIMEOUT after " + $script:StartTimeoutSec + " s: last heartbeat value was '" + $lastHb + "'. The VM is running but its guest services did not report in. Check the viewer: .\Workbench.ps1 -Action Connect") -ForegroundColor Yellow
            Set-Outcome -Result 'started-heartbeat-timeout' -Detail ("last heartbeat " + $lastHb) -Code 1
        }
    }
}

function Invoke-ShutdownAction {
    $vm = Get-TargetVm
    $state = [string]$vm.State
    if ($state -eq 'Off') {
        Write-Host 'The VM is already Off (Get-VM State = Off). Nothing to do.'
        Set-Outcome -Result 'noop-already-off' -Detail 'VM already Off'
        return
    }
    if ($state -ne 'Running') {
        Write-Host ("Refusing a graceful shutdown: the VM state is '" + $state + "' (expected Running).") -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail ("state is " + $state) -Code 1
        return
    }
    $null = Invoke-Measure -What 'Shutdown integration service' -Method 'Get-VMIntegrationService (Shutdown: Enabled, PrimaryStatusDescription)' -Probe {
        $svc = Get-IntegrationServiceByKey -EnglishName 'Shutdown' -IdSuffix $script:ShutdownId
        if ($null -eq $svc) { throw 'Shutdown integration service not found' }
        $text = "enabled {0}, status {1}" -f $svc.Enabled, (Get-PropValue -Object $svc -Name 'PrimaryStatusDescription')
        New-MeasureResult -Value $text -Display $text
    }
    Write-Host ("Asking the guest to shut down (graceful, no force). Waiting up to " + $script:ShutdownTimeoutSec + " s ...")
    $job = Stop-VM -Name $Name -Confirm:$false -AsJob
    $deadline = (Get-Date).AddSeconds($script:ShutdownTimeoutSec)
    $state = ''
    $jobError = ''
    try {
        while ($true) {
            $state = [string](Get-TargetVm).State
            Write-Tagged -Tag '[MEASURED]' -Text ("State={0} (Stop-VM job: {1})" -f $state, $job.State) -Color 'Cyan'
            Write-Detail ('method: Get-VM (State); job state | at ' + (Get-Stamp))
            if ($state -eq 'Off') { break }
            if ($job.State -eq 'Failed') {
                try { Receive-Job -Job $job -ErrorAction Stop | Out-Null } catch { $jobError = $_.Exception.Message }
                break
            }
            if ((Get-Date) -ge $deadline) { break }
            Start-Sleep -Seconds $script:PollSec
        }
    }
    finally {
        # Removes only the PowerShell job object; it does not affect the guest.
        Remove-Job -Job $job -Force -ErrorAction SilentlyContinue
    }
    if ($state -eq 'Off') {
        Write-Host 'The VM is Off.'
        Set-Outcome -Result 'shutdown-complete' -Detail 'State Off'
    }
    elseif ($jobError) {
        Write-Host ('Graceful shutdown failed: ' + (Limit-Text -Text $jobError) + ' Nothing was forced.') -ForegroundColor Red
        Write-Host 'Options: sign in to the VM and shut Windows down there, or (power-off, like pulling the plug): .\Workbench.ps1 -Action Stop -Force'
        Set-Outcome -Result 'shutdown-failed' -Detail (Limit-Text -Text $jobError) -Code 1
    }
    else {
        Write-Host ("TIMEOUT after " + $script:ShutdownTimeoutSec + " s: the VM is still '" + $state + "'. Nothing was forced.") -ForegroundColor Yellow
        Write-Host 'Options: wait and run -Action Status, shut Windows down inside the VM, or (power-off, like pulling the plug): .\Workbench.ps1 -Action Stop -Force'
        Set-Outcome -Result 'shutdown-timeout' -Detail ("state " + $state) -Code 1
    }
}

function Invoke-StopAction {
    $vm = Get-TargetVm
    $state = [string]$vm.State
    if (-not $Force) {
        Write-Host '-Action Stop is a POWER-OFF, like pulling the plug: unsaved work inside the VM is lost and Windows may need to repair its disk.' -ForegroundColor Red
        Write-Host 'Nothing was done. For a clean stop use: .\Workbench.ps1 -Action Shutdown'
        Write-Host 'If you really want the power-off, repeat the command with -Force.'
        Set-Outcome -Result 'refused-no-force' -Detail 'Stop needs -Force' -Code 1
        return
    }
    if ($state -eq 'Off') {
        Write-Host 'The VM is already Off (Get-VM State = Off). Nothing to do.'
        Set-Outcome -Result 'noop-already-off' -Detail 'VM already Off'
        return
    }
    Write-Host 'WARNING: powering the VM off now (like pulling the plug).' -ForegroundColor Red
    Stop-VM -Name $Name -TurnOff -Confirm:$false
    $final = Wait-VmState -Wanted 'Off' -TimeoutSec 60
    $null = Invoke-Measure -What 'State after power-off' -Method 'Get-VM (State)' -Probe { New-MeasureResult -Value $final -Display $final }
    if ($final -eq 'Off') { Set-Outcome -Result 'power-off-complete' -Detail 'State Off' }
    else { Set-Outcome -Result 'power-off-not-confirmed' -Detail ("state " + $final) -Code 1 }
}

function Get-CheckpointLabel {
    # Keeps the label to safe characters so the checkpoint name is easy to type back.
    param([string]$Text)
    if ([string]::IsNullOrWhiteSpace($Text)) { return '' }
    $clean = ($Text -replace '[^A-Za-z0-9 ._-]', '').Trim()
    if ($clean.Length -gt 60) { $clean = $clean.Substring(0, 60).Trim() }
    return $clean
}

function Invoke-CheckpointAction {
    $null = Get-TargetVm
    $stamp = (Get-Date).ToString('dd-MM-yyyy HHmm', $script:Culture)
    $text = Get-CheckpointLabel -Text $Label
    $snapName = $stamp
    if ($text) { $snapName = $stamp + ' ' + $text }
    Write-Host ("Creating checkpoint '" + $snapName + "' ...")
    Checkpoint-VM -Name $Name -SnapshotName $snapName
    $check = Invoke-Measure -What 'New checkpoint exists' -Method 'Get-VMSnapshot (match on Name)' -Probe {
        $found = @(Get-SortedSnapshots | Where-Object { $_.Name -ceq $snapName })
        if ($found.Count -eq 0) { throw 'checkpoint not found after Checkpoint-VM' }
        $line = Format-SnapshotLine -Snapshot $found[0]
        New-MeasureResult -Value $line -Display $line
    }
    Show-CheckpointList
    if ($check.Status -eq 'MEASURED') { Set-Outcome -Result 'checkpoint-created' -Detail $snapName }
    else { Set-Outcome -Result 'checkpoint-not-confirmed' -Detail $snapName -Code 1 }
}

function Invoke-CheckpointsAction {
    $null = Get-TargetVm
    Show-CheckpointList
    Set-Outcome -Result 'ok' -Detail 'checkpoints listed'
}

function Invoke-RestoreAction {
    $vm = Get-TargetVm
    if ([string]::IsNullOrWhiteSpace($CheckpointName)) {
        Write-Host '-Action Restore needs -CheckpointName "<exact name>". List the names with: .\Workbench.ps1 -Action Checkpoints' -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail 'missing -CheckpointName' -Code 1
        return
    }
    $matching = @(Get-SortedSnapshots | Where-Object { $_.Name -ceq $CheckpointName })
    if ($matching.Count -ne 1) {
        Write-Host ("Found " + $matching.Count + " checkpoint(s) named exactly '" + $CheckpointName + "'; need exactly 1. Nothing was done.") -ForegroundColor Red
        Show-CheckpointList
        Set-Outcome -Result 'refused' -Detail ("checkpoint matches: " + $matching.Count) -Code 1
        return
    }
    $snap = $matching[0]
    $line = Format-SnapshotLine -Snapshot $snap
    if (-not $Force) {
        Write-Host ("WOULD restore VM '" + $Name + "' (now " + $vm.State + ") to checkpoint: " + $line) -ForegroundColor Yellow
        Write-Host 'Everything done in the VM since that checkpoint would be LOST, and a running VM would be stopped first.' -ForegroundColor Yellow
        Write-Host 'NOTHING WAS DONE. To do it, repeat the same command with -Force.'
        Set-Outcome -Result 'refused-no-force' -Detail ("would restore " + $CheckpointName) -Code 1
        return
    }
    Write-Host ("Restoring '" + $Name + "' to: " + $line) -ForegroundColor Yellow
    Restore-VMSnapshot -VMSnapshot $snap -Confirm:$false
    $null = Invoke-Measure -What 'State after restore' -Method 'Get-VM (State)' -Probe {
        $s = [string](Get-TargetVm).State
        New-MeasureResult -Value $s -Display $s
    }
    Set-Outcome -Result 'restored' -Detail $CheckpointName
}

function Invoke-IpAction {
    $vm = Get-TargetVm
    if ([string]$vm.State -ne 'Running') {
        Write-Host ("The VM is '" + $vm.State + "', not Running, so it has no IP address to read. Start it first: .\Workbench.ps1 -Action Start") -ForegroundColor Yellow
        Set-Outcome -Result 'no-ip-vm-not-running' -Detail ("state " + $vm.State) -Code 1
        return
    }
    $ipRec = Invoke-Measure -What 'First usable IPv4 address' -Method '(Get-VMNetworkAdapter).IPAddresses (first IPv4, 169.254.x.x skipped)' -Probe {
        $ip = Get-FirstUsableIPv4 -Addresses @(Get-VmIpAddresses)
        if (-not $ip) { throw 'no IPv4 address reported yet (guest integration services need a minute after boot)' }
        New-MeasureResult -Value $ip -Display $ip
    }
    if ($ipRec.Status -ne 'MEASURED') {
        Set-Outcome -Result 'no-ip' -Detail 'no IPv4 reported' -Code 1
        return
    }
    $ip = [string]$ipRec.Value
    Write-Host ('Host-side driver address for the guest hands service: ' + $ip + ':' + $Port)
    $tcp = Invoke-Measure -What ("TCP port " + $Port + " on " + $ip) -Method 'Test-NetConnection -ComputerName <ip> -Port (TcpTestSucceeded)' -Probe {
        $r = Test-NetConnection -ComputerName $ip -Port $Port -WarningAction SilentlyContinue
        New-MeasureResult -Value ([bool]$r.TcpTestSucceeded) -Display ([string][bool]$r.TcpTestSucceeded)
    }
    if ($tcp.Status -eq 'MEASURED' -and $tcp.Value -eq $false) {
        Write-Host ("Port " + $Port + " did not answer. That is expected until the guest hands service is installed and running inside the VM.") -ForegroundColor Yellow
    }
    $detail = "ip " + $ip + ", port " + $Port + " reachable: " + [string]$tcp.Value
    Set-Outcome -Result 'ip-read' -Detail $detail
}

function Invoke-ConnectAction {
    $null = Get-TargetVm
    $cmd = Get-Command -Name 'vmconnect.exe' -ErrorAction SilentlyContinue
    if ($null -eq $cmd) {
        Write-Host 'vmconnect.exe was not found (Hyper-V Management Tools are not installed).' -ForegroundColor Red
        Set-Outcome -Result 'failed' -Detail 'vmconnect.exe not found' -Code 1
        return
    }
    $proc = Start-Process -FilePath $cmd.Source -ArgumentList ('localhost "' + $Name + '"') -PassThru
    Start-Sleep -Seconds 2
    $null = Invoke-Measure -What 'Viewer process' -Method 'Start-Process -PassThru; Process.HasExited after 2 s' -Probe {
        $text = "vmconnect.exe process id {0}, exited: {1}" -f $proc.Id, $proc.HasExited
        New-MeasureResult -Value (-not $proc.HasExited) -Display $text
    }
    Write-Host 'NOTE: this window is the viewer (Desktop 1). Clicking inside it sends YOUR keyboard to the VM. Click outside it to give focus back to the host.'
    Write-Host 'NOTE: the agent''s input goes through the guest service, not through this viewer.'
    Set-Outcome -Result 'viewer-launched' -Detail ('vmconnect pid ' + $proc.Id)
}

function Invoke-CopyAction {
    $vm = Get-TargetVm
    if ([string]::IsNullOrWhiteSpace($Source) -or [string]::IsNullOrWhiteSpace($Destination)) {
        Write-Host '-Action Copy needs -Source <host file> and -Destination <full guest file path>.' -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail 'missing -Source or -Destination' -Code 1
        return
    }
    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        Write-Host ("-Source is not an existing file: " + $Source) -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail 'source file missing' -Code 1
        return
    }
    if ($Destination.EndsWith('\') -or $Destination.EndsWith('/')) {
        Write-Host '-Destination must be a full file path including the file name, not a folder.' -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail 'destination is a folder' -Code 1
        return
    }
    if ([string]$vm.State -ne 'Running') {
        Write-Host ("The VM is '" + $vm.State + "'; it must be Running to receive files.") -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail ("state " + $vm.State) -Code 1
        return
    }
    $gsi = Invoke-Measure -What 'Guest Service Interface' -Method 'Get-VMIntegrationService (Enabled, PrimaryStatusDescription)' -Probe {
        $svc = Get-IntegrationServiceByKey -EnglishName 'Guest Service Interface' -IdSuffix $script:GuestServiceId
        if ($null -eq $svc) { throw 'Guest Service Interface not found' }
        $status = [string](Get-PropValue -Object $svc -Name 'PrimaryStatusDescription')
        $info = [pscustomobject]([ordered]@{ Enabled = [bool]$svc.Enabled; Status = $status; Name = [string]$svc.Name })
        New-MeasureResult -Value $info -Display ("enabled {0}, status {1}" -f $info.Enabled, $info.Status)
    }
    if ($gsi.Status -ne 'MEASURED' -or -not $gsi.Value.Enabled) {
        Write-Host 'Guest Service Interface is not enabled. Enable it (VM stays running) with: Enable-VMIntegrationService -VMName "<name>" -Name "Guest Service Interface"' -ForegroundColor Red
        Set-Outcome -Result 'refused' -Detail 'Guest Service Interface not enabled' -Code 1
        return
    }
    $sourceFull = (Resolve-Path -LiteralPath $Source).ProviderPath
    $sourceName = [System.IO.Path]::GetFileName($sourceFull)
    $sizeKb = [math]::Round((Get-Item -LiteralPath $sourceFull).Length / 1KB, 1)
    Write-Host ("Copying " + $sourceName + " (" + $sizeKb.ToString('0.0', $script:Culture) + " KB) to the guest path " + $Destination + " ...")
    $copy = Invoke-Measure -What 'Copy-VMFile result' -Method 'Copy-VMFile -FileSource Host -CreateFullPath (completes without error or throws)' -Probe {
        if ($Overwrite) { Copy-VMFile -VMName $Name -SourcePath $sourceFull -DestinationPath $Destination -FileSource Host -CreateFullPath -Force }
        else { Copy-VMFile -VMName $Name -SourcePath $sourceFull -DestinationPath $Destination -FileSource Host -CreateFullPath }
        New-MeasureResult -Value $true -Display 'Copy-VMFile returned without error'
    }
    if ($copy.Status -eq 'MEASURED') {
        Write-Host 'Arrival inside the guest is NOT MEASURED: the host cannot read the guest disk. Check the file inside the VM.' -ForegroundColor Yellow
        Set-Outcome -Result 'copy-cmdlet-ok' -Detail ($sourceName + ' -> ' + $Destination + ' (arrival not verified)')
    }
    else {
        Write-Host 'If the error says the file exists, add -Overwrite. If it says the service is unavailable, the guest integration services are not running yet.' -ForegroundColor Yellow
        Set-Outcome -Result 'copy-failed' -Detail ($sourceName + ' -> ' + $Destination + ': ' + (Limit-Text -Text $copy.Display -Max 200)) -Code 1
    }
}

function Initialize-HyperV {
    # Returns $true when the Hyper-V module is usable and this account may manage VMs.
    if (-not (Test-CanManageHyperV)) {
        Write-Host 'This needs an elevated PowerShell (or membership of the Hyper-V Administrators group). Open one: Start menu, type PowerShell, right-click Windows PowerShell, Run as administrator.' -ForegroundColor Red
        Set-Outcome -Result 'refused-not-admin' -Detail 'not administrator' -Code 1
        return $false
    }
    try {
        Import-Module -Name 'Hyper-V' -ErrorAction Stop | Out-Null
    }
    catch {
        Write-Host ('The Hyper-V PowerShell module could not be loaded: ' + (Limit-Text -Text $_.Exception.Message) + ' Run Preflight-Workbench.ps1.') -ForegroundColor Red
        Set-Outcome -Result 'failed-no-hyperv-module' -Detail (Limit-Text -Text $_.Exception.Message) -Code 1
        return $false
    }
    return $true
}

function Invoke-Action {
    switch ($Action) {
        'Status'      { Invoke-StatusAction }
        'Start'       { Invoke-StartAction }
        'Shutdown'    { Invoke-ShutdownAction }
        'Stop'        { Invoke-StopAction }
        'Checkpoint'  { Invoke-CheckpointAction }
        'Checkpoints' { Invoke-CheckpointsAction }
        'Restore'     { Invoke-RestoreAction }
        'Ip'          { Invoke-IpAction }
        'Connect'     { Invoke-ConnectAction }
        'Copy'        { Invoke-CopyAction }
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
try {
    Write-Host ('AVNI Agent Workbench - ' + $Action + " - '" + $Name + "' - " + (Get-Stamp)) -ForegroundColor White
    if (Initialize-HyperV) { Invoke-Action }
}
catch {
    $message = Limit-Text -Text $_.Exception.Message -Max 400
    Write-Host ('FAILED: ' + $message) -ForegroundColor Red
    Set-Outcome -Result 'error' -Detail $message -Code 1
}
finally {
    Write-OpsLog
}
exit $script:ExitCode
