#Requires -Version 5.1
<#
.SYNOPSIS
Proves from INSIDE the guest (PowerShell Direct) that the AVNI Agent Workbench VM cannot reach the owner's LAN but can reach the internet.

.DESCRIPTION
Companion to Set-WorkbenchNetworkIsolation.ps1. That script puts Hyper-V Extended Port ACLs
on the VM's adapter; this one measures what the guest can actually reach. It needs no guest
network at all to get in: it uses PowerShell Direct (Invoke-Command -VMName), which travels
over the Hyper-V VM bus. The adapter must however be CONNECTED, otherwise every LAN test
would "pass" for the wrong reason; in that case the verdict is NOT MEASURED.

Inside the guest it runs Test-NetConnection:
  - every LAN target on TCP 445 and TCP 3389 and ICMP         (expected: no answer = good)
  - every internet target on TCP 443                           (expected: success = good)
  - Resolve-DnsName for the DNS name                           (expected: success = good)
  - reads the guest's own IPv4 addresses and default gateway, whether TCP 7001 is listening,
    and (information only) TCP 445 / 3389 on its default gateway, which is the host.
On the host it then runs Test-NetConnection to the guest IPv4 on TCP 7001 and reads the
Extended ACL rules. Port 7001 is reported as MEASURED information, not as an isolation pass/fail:
"not listening (service not installed yet)" is a normal state before the hands service exists.

Honest limits, printed again in the output:
  - A blocked SYN takes about 21 s to time out, and Test-NetConnection has no timeout switch.
    With the defaults expect a few minutes. The run is capped at 15 minutes.
  - "No answer" cannot tell the switch ACL apart from the target's own firewall dropping the
    packet. A FAST failure (under 5 s) means something answered (a closed port sends a reset),
    which counts as a leak. The ACL itself is proven by the host-side read-back row.
  - A ping with no reply is weak evidence: many hosts never answer ping.

Verdict (exit code):
  ISOLATION VERIFIED  (0)  every row PASS.
  ISOLATION FAILED    (1)  a LAN target answered, or a LAN/internet/ACL row failed.
  NOT MEASURED        (2)  VM not Running, adapter not connected, PowerShell Direct refused,
                           not administrator, guest timed out, or the guest has no working
                           internet at all (then "blocked" results prove nothing).
It never starts or stops the VM and never changes any setting. It writes one JSON report to
%LOCALAPPDATA%\Avni\reports\workbench-isolation-test-dd-MM-yyyy_HHmm.json (the guest password is
never stored; only the user name is recorded).

PowerShell Direct needs a Windows 10/11 or Server 2016+ guest that has finished first sign-in
set-up and a local account WITH a password. Needs an elevated PowerShell on the host.

.PARAMETER Name
VM name. Default 'AVNI Agent Workbench 01'.

.PARAMETER LanTargets
LAN IPv4 addresses that MUST be unreachable. Default: the host's own LAN IPv4 addresses (measured
on the host) plus the host's default gateway on the LAN.

.PARAMETER InternetTargets
Internet IPv4 addresses tested on TCP 443. Default 1.1.1.1 and 8.8.8.8.

.PARAMETER DnsName
Name resolved inside the guest. Default www.microsoft.com.

.PARAMETER Port
TCP port of the guest hands service tested from the host. Default 7001.

.PARAMETER Credential
Guest account for PowerShell Direct. If omitted you are asked with Get-Credential.

.EXAMPLE
.\Test-WorkbenchIsolation.ps1

.EXAMPLE
.\Test-WorkbenchIsolation.ps1 -LanTargets 192.168.1.1,192.168.1.50
#>
[CmdletBinding()]
param(
    [ValidatePattern('^[A-Za-z0-9]([A-Za-z0-9 ._-]{0,62}[A-Za-z0-9])?$')]
    [string]$Name = 'AVNI Agent Workbench 01',

    [string[]]$LanTargets,

    [string[]]$InternetTargets = @('1.1.1.1', '8.8.8.8'),

    [ValidateNotNullOrEmpty()]
    [string]$DnsName = 'www.microsoft.com',

    [ValidateRange(1, 65535)]
    [int]$Port = 7001,

    [System.Management.Automation.PSCredential]$Credential
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Script-wide state and constants
# ---------------------------------------------------------------------------
$script:ToolName        = 'Test-WorkbenchIsolation'
$script:ToolVersion     = '1.0.0'
$script:Culture         = [System.Globalization.CultureInfo]::InvariantCulture
$script:Started         = Get-Date
$script:Records         = New-Object System.Collections.Generic.List[object]
$script:Rows            = New-Object System.Collections.Generic.List[object]
$script:Warnings        = New-Object System.Collections.Generic.List[string]
$script:FastFailSec     = 5
$script:GuestTimeoutSec = 900
$script:InternetPort    = 443
$script:PrivateV4       = @('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '169.254.0.0/16', '100.64.0.0/10')
$script:GuestUser       = ''

# ---------------------------------------------------------------------------
# Helpers (house style)
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

function Limit-Text {
    param([string]$Text, [int]$Max = 200)
    if ($null -eq $Text) { return '' }
    $flat = ($Text -replace '\s+', ' ').Trim()
    if ($flat.Length -le $Max) { return $flat }
    return $flat.Substring(0, $Max - 3) + '...'
}

function Test-CanManageHyperV {
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

function Add-Warning {
    param([string]$Text)
    $script:Warnings.Add($Text)
    Write-Tagged -Tag '[WARNING]' -Text $Text -Color 'Yellow'
}

function New-MeasureResult {
    param($Value, [string]$Display = '')
    $result = [pscustomobject]@{ Value = $Value; Display = $Display }
    $result.PSObject.TypeNames.Insert(0, 'Avni.MeasureResult')
    return $result
}

function Invoke-Measure {
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
    $script:Records.Add($record)
    if ($mxStatus -eq 'MEASURED') { Write-Tagged -Tag '[MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color $Color }
    else { Write-Tagged -Tag '[NOT MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color 'Yellow' }
    Write-Detail ('method: ' + $Method + ' | at ' + $mxAt)
    return $record
}

function Add-Row {
    # One verdict-table row. Result is PASS, FAIL, INFO or NOT MEASURED.
    param([string]$Group, [string]$Target, [string]$Check, [string]$Expected, [string]$Measured, [string]$Result)
    $script:Rows.Add([pscustomobject]([ordered]@{
        Group = $Group; Target = $Target; Check = $Check; Expected = $Expected; Measured = $Measured; Result = $Result
    }))
}

function Get-ReportDirectory {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
    return (Join-Path -Path $env:LOCALAPPDATA -ChildPath 'Avni\reports')
}

function Save-Report {
    param([string]$Verdict, [int]$Code, [string]$Reason, $Extra)
    try {
        $dir = Get-ReportDirectory
        [System.IO.Directory]::CreateDirectory($dir) | Out-Null
        $stamp = $script:Started.ToString('dd-MM-yyyy_HHmm', $script:Culture)
        $base = Join-Path -Path $dir -ChildPath ('workbench-isolation-test-' + $stamp)
        $candidate = $base
        $n = 1
        while (Test-Path -LiteralPath ($candidate + '.json')) { $n++; $candidate = $base + '-' + $n }
        $record = [ordered]@{
            Tool         = $script:ToolName
            ToolVersion  = $script:ToolVersion
            Vm           = $Name
            GuestUser    = $script:GuestUser
            StartedAt    = $script:Started.ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            FinishedAt   = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            Verdict      = $Verdict
            Reason       = (Limit-Text -Text $Reason -Max 500)
            ExitCode     = $Code
            Rows         = $script:Rows.ToArray()
            Measurements = $script:Records.ToArray()
            Warnings     = $script:Warnings.ToArray()
            Extra        = $Extra
        }
        $json = ConvertTo-Json -InputObject $record -Depth 8
        $path = $candidate + '.json'
        [System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host ''
        Write-Host ('Report written: ' + $path) -ForegroundColor White
    }
    catch {
        Write-Host ('WARNING: could not write the report: ' + $_.Exception.Message) -ForegroundColor Yellow
    }
}

function Complete-Run {
    # Prints the verdict table and the overall line, writes the report, exits.
    param([string]$Verdict, [int]$Code, [string]$Reason, $Extra = $null)
    Write-Host ''
    if ($script:Rows.Count -gt 0) {
        Write-Host 'VERDICT TABLE (expected vs measured):' -ForegroundColor White
        $text = ($script:Rows | Select-Object Group, Target, Check, Expected, Measured, Result | Format-Table -AutoSize -Wrap | Out-String)
        foreach ($line in ($text -split "`r?`n")) { if ($line.Trim().Length -gt 0) { Write-Host ('  ' + $line) } }
    }
    $color = 'Red'
    if ($Verdict -eq 'ISOLATION VERIFIED') { $color = 'Green' } elseif ($Verdict -eq 'NOT MEASURED') { $color = 'Yellow' }
    Write-Host ''
    Write-Host ($Verdict + ' - ' + $Reason) -ForegroundColor $color
    Write-Host 'Limit: "no answer" cannot tell the switch ACL from the target''s own firewall; the ACL itself is proven by the host-side read-back row and by Set-WorkbenchNetworkIsolation.ps1 -Action Show.'
    Save-Report -Verdict $Verdict -Code $Code -Reason $Reason -Extra $Extra
    exit $Code
}

# ---------------------------------------------------------------------------
# Host-side lookups
# ---------------------------------------------------------------------------
function Get-HostLanAddressList {
    $all = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop | Where-Object {
        ([string]$_.PrefixOrigin -in @('Dhcp', 'Manual')) -and
        ([string]$_.InterfaceAlias -notlike 'vEthernet*') -and
        ([string]$_.InterfaceAlias -notlike '*Loopback*') -and
        ([string]$_.IPAddress -notlike '127.*')
    })
    return @($all | ForEach-Object { [string]$_.IPAddress } | Select-Object -Unique)
}

function Get-HostLanGateway {
    # Default route of the host that does not go through a vEthernet adapter, lowest metric first.
    $routes = @(Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction Stop |
        Where-Object { ([string]$_.InterfaceAlias -notlike 'vEthernet*') -and ([string]$_.NextHop -ne '0.0.0.0') } |
        Sort-Object -Property RouteMetric)
    if ($routes.Count -eq 0) { throw 'no default route outside vEthernet adapters' }
    return [string]$routes[0].NextHop
}

function Get-AclBaselineState {
    # Do the five private IPv4 ranges have an Outbound and an Inbound Deny rule on the adapter?
    $rules = @(Get-VMNetworkAdapterExtendedAcl -VMName $Name -ErrorAction Stop)
    $missing = New-Object System.Collections.Generic.List[string]
    foreach ($r in $script:PrivateV4) {
        foreach ($d in @('Outbound', 'Inbound')) {
            $hit = @($rules | Where-Object {
                ([string](Get-PropValue -Object $_ -Name 'Direction') -eq $d) -and
                ([string](Get-PropValue -Object $_ -Name 'Action') -eq 'Deny') -and
                (([string](Get-PropValue -Object $_ -Name 'RemoteIPAddress')).Trim().ToLowerInvariant() -eq $r)
            })
            if ($hit.Count -eq 0) { $missing.Add($d + ' Deny ' + $r) }
        }
    }
    return [pscustomobject]([ordered]@{ RuleCount = $rules.Count; Missing = $missing.ToArray() })
}

# ---------------------------------------------------------------------------
# What runs inside the guest (Windows PowerShell 5.1 compatible)
# ---------------------------------------------------------------------------
$script:GuestBlock = {
    param($LanTargets, $InternetTargets, $DnsName, $HandsPort, $InternetPort)
    $ErrorActionPreference = 'Continue'
    function Probe-Tcp {
        param($Target, $PortNumber)
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $ok = $false
        $err = ''
        try { $ok = [bool](Test-NetConnection -ComputerName $Target -Port $PortNumber -InformationLevel Quiet -WarningAction SilentlyContinue -ErrorAction Stop) }
        catch { $err = $_.Exception.Message }
        $sw.Stop()
        return [pscustomobject]@{ Target = [string]$Target; Port = [int]$PortNumber; Proto = 'TCP'; Success = $ok; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Error = $err }
    }
    function Probe-Icmp {
        param($Target)
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $ok = $false
        $err = ''
        try { $ok = [bool](Test-NetConnection -ComputerName $Target -InformationLevel Quiet -WarningAction SilentlyContinue -ErrorAction Stop) }
        catch { $err = $_.Exception.Message }
        $sw.Stop()
        return [pscustomobject]@{ Target = [string]$Target; Port = 0; Proto = 'ICMP'; Success = $ok; Seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1); Error = $err }
    }
    $ips = @()
    try { $ips = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop | Where-Object { $_.IPAddress -notlike '127.*' } | ForEach-Object { [string]$_.IPAddress }) } catch { }
    $gw = ''
    try {
        $route = @(Get-NetRoute -AddressFamily IPv4 -DestinationPrefix '0.0.0.0/0' -ErrorAction Stop | Where-Object { $_.NextHop -ne '0.0.0.0' } | Sort-Object -Property RouteMetric)
        if ($route.Count -gt 0) { $gw = [string]$route[0].NextHop }
    } catch { }
    $listening = $false
    try { $listening = (@(Get-NetTCPConnection -State Listen -LocalPort $HandsPort -ErrorAction Stop).Count -gt 0) } catch { }

    $lan = New-Object System.Collections.ArrayList
    foreach ($t in @($LanTargets)) {
        [void]$lan.Add((Probe-Tcp -Target $t -PortNumber 445))
        [void]$lan.Add((Probe-Tcp -Target $t -PortNumber 3389))
        [void]$lan.Add((Probe-Icmp -Target $t))
    }
    $inet = New-Object System.Collections.ArrayList
    foreach ($t in @($InternetTargets)) { [void]$inet.Add((Probe-Tcp -Target $t -PortNumber $InternetPort)) }
    $dnsOk = $false
    $dnsText = ''
    try {
        $d = @(Resolve-DnsName -Name $DnsName -Type A -ErrorAction Stop | Where-Object { $_.PSObject.Properties.Match('IPAddress').Count -gt 0 })
        $dnsOk = ($d.Count -gt 0)
        $dnsText = (@($d | ForEach-Object { [string]$_.IPAddress }) -join ', ')
    } catch { $dnsText = $_.Exception.Message }
    $gwProbes = New-Object System.Collections.ArrayList
    if ($gw) {
        [void]$gwProbes.Add((Probe-Tcp -Target $gw -PortNumber 445))
        [void]$gwProbes.Add((Probe-Tcp -Target $gw -PortNumber 3389))
    }
    return [pscustomobject]@{
        GuestMarker = $true
        GuestIPv4 = @($ips); GuestGateway = $gw; HandsListening = $listening
        Lan = @($lan); Internet = @($inet); DnsOk = $dnsOk; DnsText = $dnsText; GatewayProbes = @($gwProbes)
        ComputerName = $env:COMPUTERNAME
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
Write-Host ('AVNI Agent Workbench - isolation test - ' + "'" + $Name + "' - " + (Get-Stamp)) -ForegroundColor White
Write-Host 'This never starts or stops the VM and changes no setting. It measures what the guest can reach.'
Write-Host ''

if (-not (Test-CanManageHyperV)) {
    Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason 'not an elevated administrator (or Hyper-V Administrators member); nothing was measured'
}
try { Import-Module -Name 'Hyper-V' -ErrorAction Stop | Out-Null }
catch { Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason ('the Hyper-V module could not be loaded: ' + (Limit-Text -Text $_.Exception.Message)) }

$vmRec = Invoke-Measure -What 'VM State' -Method 'Get-VM -Name (State)' -Probe {
    $found = @(Get-VM -Name $Name -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { throw ("no VM named '" + $Name + "'") }
    New-MeasureResult -Value ([string]$found[0].State) -Display ([string]$found[0].State)
}
if ($vmRec.Status -ne 'MEASURED') { Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason $vmRec.Display }
if ($vmRec.Value -ne 'Running') {
    Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason ("the VM is '" + $vmRec.Value + "', not Running. This script never starts it. Start it yourself (.\Workbench.ps1 -Action Start) after Set-WorkbenchNetworkIsolation.ps1 -Action Apply and -Action Connect, then repeat.")
}

$adRec = Invoke-Measure -What 'Adapter SwitchName / Connected' -Method 'Get-VMNetworkAdapter (SwitchName, Connected)' -Probe {
    $a = @(Get-VMNetworkAdapter -VMName $Name)
    if ($a.Count -ne 1) { throw ('expected 1 adapter, found ' + $a.Count) }
    $c = Get-PropValue -Object $a[0] -Name 'Connected'
    if ($null -eq $c) { throw 'Hyper-V reported no Connected value' }
    $info = [pscustomobject]@{ SwitchName = [string]$a[0].SwitchName; Connected = [bool]$c }
    New-MeasureResult -Value $info -Display ("SwitchName = '{0}', Connected = {1}" -f $info.SwitchName, $info.Connected)
}
if ($adRec.Status -ne 'MEASURED') { Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason $adRec.Display }
if (-not $adRec.Value.Connected) {
    Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason 'the adapter is disconnected: every LAN target would be unreachable for the wrong reason. Run Set-WorkbenchNetworkIsolation.ps1 -Action Connect (after Apply), then repeat.'
}

$aclRec = Invoke-Measure -What 'Host-side Extended ACL baseline (5 private ranges, Outbound and Inbound Deny)' -Method 'Get-VMNetworkAdapterExtendedAcl -VMName' -Probe {
    $s = Get-AclBaselineState
    $txt = $s.RuleCount.ToString() + ' rule(s)'
    if ($s.Missing.Count -gt 0) { $txt = $txt + '; MISSING: ' + ($s.Missing -join ', ') }
    New-MeasureResult -Value $s -Display $txt
}
if ($aclRec.Status -eq 'MEASURED' -and $aclRec.Value.Missing.Count -eq 0) {
    Add-Row -Group 'Host ACL' -Target $Name -Check 'deny rules present' -Expected '10 deny rules' -Measured 'all present' -Result 'PASS'
}
elseif ($aclRec.Status -eq 'MEASURED') {
    Add-Row -Group 'Host ACL' -Target $Name -Check 'deny rules present' -Expected '10 deny rules' -Measured ($aclRec.Value.Missing.Count.ToString() + ' missing') -Result 'FAIL'
}
else {
    Add-Row -Group 'Host ACL' -Target $Name -Check 'deny rules present' -Expected '10 deny rules' -Measured 'NOT MEASURED' -Result 'NOT MEASURED'
}

# Default LAN targets, measured on the host.
$lanList = @()
if ($PSBoundParameters.ContainsKey('LanTargets') -and @($LanTargets).Count -gt 0) { $lanList = @($LanTargets) }
else {
    $lanRec = Invoke-Measure -What "Host LAN IPv4 addresses and LAN default gateway (default LAN targets)" -Method 'Get-NetIPAddress; Get-NetRoute 0.0.0.0/0 (vEthernet excluded)' -Probe {
        $ips = @(Get-HostLanAddressList)
        $gwText = ''
        try { $gwText = Get-HostLanGateway } catch { $gwText = '' }
        $all = @($ips)
        if ($gwText) { $all = @($all) + @($gwText) }
        $all = @($all | Select-Object -Unique)
        if ($all.Count -eq 0) { throw 'no host LAN address or gateway found' }
        New-MeasureResult -Value $all -Display ($all -join ', ')
    }
    if ($lanRec.Status -ne 'MEASURED') { Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason 'no LAN targets: pass -LanTargets or fix the host network lookup' }
    $lanList = @($lanRec.Value)
}
$inetList = @($InternetTargets)

$cred = $Credential
if ($null -eq $cred) {
    Write-Host 'PowerShell Direct needs the guest account. A prompt follows; the password is kept only in memory and is never written anywhere.'
    try { $cred = Get-Credential -Message ("Local account inside '" + $Name + "' (PowerShell Direct)") } catch { $cred = $null }
}
if ($null -eq $cred) { Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason 'no guest credential supplied' }
$script:GuestUser = [string]$cred.UserName

$probeCount = ($lanList.Count * 3) + $inetList.Count + 3
Write-Host ''
Write-Host ('Running ' + $probeCount + ' probes inside the guest. A blocked connection takes about 21 s to time out and Test-NetConnection has no timeout switch, so worst case is roughly ' + [int]($lanList.Count * 2 * 21 + $lanList.Count * 8 + $inetList.Count * 21 + 60) + ' s. The run is capped at ' + $script:GuestTimeoutSec + ' s.') -ForegroundColor White

$guest = $null
$job = $null
$guestErr = ''
try {
    $job = Invoke-Command -VMName $Name -Credential $cred -ScriptBlock $script:GuestBlock -ArgumentList @(@($lanList), @($inetList), $DnsName, $Port, $script:InternetPort) -AsJob -ErrorAction Stop
    $done = Wait-Job -Job $job -Timeout $script:GuestTimeoutSec
    if ($null -eq $done -or $job.State -eq 'Running') {
        Stop-Job -Job $job -ErrorAction SilentlyContinue
        $guestErr = 'the guest probes did not finish within ' + $script:GuestTimeoutSec + ' s'
    }
    else {
        $out = @(Receive-Job -Job $job -ErrorAction Stop)
        $found = @($out | Where-Object { $null -ne $_ -and $_.PSObject.Properties.Match('GuestMarker').Count -gt 0 })
        if ($found.Count -eq 0) { $guestErr = 'the guest returned no result object' } else { $guest = $found[$found.Count - 1] }
    }
}
catch {
    $guestErr = 'PowerShell Direct failed: ' + (Limit-Text -Text $_.Exception.Message -Max 250)
}
finally {
    # Removes only the host's PowerShell job object; it does not affect the guest.
    if ($null -ne $job) { Remove-Job -Job $job -Force -ErrorAction SilentlyContinue }
}
$measuredAt = Get-Stamp
if ($null -eq $guest) {
    Write-Tagged -Tag '[NOT MEASURED]' -Text ('Guest probes: ' + $guestErr) -Color 'Yellow'
    Write-Detail 'common causes: wrong guest password or blank password, guest still in first-run setup, guest not fully started, VM heartbeat not yet OK'
    Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason $guestErr
}
Write-Tagged -Tag '[MEASURED]' -Text ("Guest '" + (Get-PropValue -Object $guest -Name 'ComputerName') + "' IPv4: " + (@($guest.GuestIPv4) -join ', ') + ' | default gateway: ' + $guest.GuestGateway) -Color 'Cyan'
Write-Detail ('method: Get-NetIPAddress / Get-NetRoute inside the guest via PowerShell Direct | at ' + $measuredAt)

# ---- Build rows from the guest results ----
$lanLeak = $false
foreach ($r in @($guest.Lan)) {
    $label = $r.Proto
    if ($r.Proto -eq 'TCP') { $label = 'TCP ' + $r.Port }
    if ($r.Success) {
        $lanLeak = $true
        Add-Row -Group 'LAN' -Target $r.Target -Check $label -Expected 'no answer' -Measured ('ANSWERED in ' + $r.Seconds + ' s') -Result 'FAIL'
    }
    elseif ($r.Proto -eq 'TCP' -and $r.Seconds -lt $script:FastFailSec) {
        Add-Row -Group 'LAN' -Target $r.Target -Check $label -Expected 'no answer (timeout)' -Measured ('failed fast in ' + $r.Seconds + ' s: something replied (reset/unreachable)') -Result 'FAIL'
    }
    elseif ($r.Proto -eq 'TCP') {
        Add-Row -Group 'LAN' -Target $r.Target -Check $label -Expected 'no answer (timeout)' -Measured ('timed out after ' + $r.Seconds + ' s') -Result 'PASS'
    }
    else {
        Add-Row -Group 'LAN' -Target $r.Target -Check $label -Expected 'no reply' -Measured ('no reply (weak evidence), ' + $r.Seconds + ' s') -Result 'PASS'
    }
}
$internetOk = $false
foreach ($r in @($guest.Internet)) {
    if ($r.Success) {
        $internetOk = $true
        Add-Row -Group 'Internet' -Target $r.Target -Check ('TCP ' + $r.Port) -Expected 'success' -Measured ('connected in ' + $r.Seconds + ' s') -Result 'PASS'
    }
    else {
        Add-Row -Group 'Internet' -Target $r.Target -Check ('TCP ' + $r.Port) -Expected 'success' -Measured ('failed after ' + $r.Seconds + ' s') -Result 'FAIL'
    }
}
if ($guest.DnsOk) {
    $internetOk = $true
    Add-Row -Group 'Internet' -Target $DnsName -Check 'DNS A lookup' -Expected 'success' -Measured ('resolved: ' + (Limit-Text -Text $guest.DnsText -Max 60)) -Result 'PASS'
}
else {
    Add-Row -Group 'Internet' -Target $DnsName -Check 'DNS A lookup' -Expected 'success' -Measured ('failed: ' + (Limit-Text -Text $guest.DnsText -Max 60)) -Result 'FAIL'
}
foreach ($r in @($guest.GatewayProbes)) {
    $ans = 'no answer'
    if ($r.Success) { $ans = 'ANSWERED (the host is reachable on this port through the gateway address)' }
    Add-Row -Group 'Info' -Target ($r.Target + ' (guest gateway = host)') -Check ('TCP ' + $r.Port) -Expected 'information only' -Measured $ans -Result 'INFO'
}

# ---- Host to guest on the hands port ----
$guestIp = ''
foreach ($ip in @($guest.GuestIPv4)) { if ($ip -notlike '169.254.*' -and -not $guestIp) { $guestIp = $ip } }
if ($guestIp) {
    $hostTest = Invoke-Measure -What ('Host to guest ' + $guestIp + ':' + $Port) -Method 'Test-NetConnection -InformationLevel Quiet from the host' -Probe {
        $ok = [bool](Test-NetConnection -ComputerName $guestIp -Port $Port -InformationLevel Quiet -WarningAction SilentlyContinue)
        New-MeasureResult -Value $ok -Display ([string]$ok)
    }
    if ($hostTest.Status -ne 'MEASURED') {
        Add-Row -Group 'Info' -Target ($guestIp + ':' + $Port) -Check 'host to guest' -Expected 'information only' -Measured 'NOT MEASURED' -Result 'INFO'
    }
    elseif ($hostTest.Value) {
        Add-Row -Group 'Info' -Target ($guestIp + ':' + $Port) -Check 'host to guest' -Expected 'listening if the service is installed' -Measured 'listening and reachable from the host' -Result 'INFO'
    }
    elseif ($guest.HandsListening) {
        Add-Warning ('Port ' + $Port + ' is listening inside the guest but the host could not connect to ' + $guestIp + '. Check the ACL (Inbound Allow from the gateway on TCP ' + $Port + ') and the guest firewall rule.')
        Add-Row -Group 'Info' -Target ($guestIp + ':' + $Port) -Check 'host to guest' -Expected 'reachable (service is listening)' -Measured 'LISTENING IN GUEST BUT NOT REACHABLE FROM HOST' -Result 'INFO'
    }
    else {
        Add-Row -Group 'Info' -Target ($guestIp + ':' + $Port) -Check 'host to guest' -Expected 'listening if the service is installed' -Measured 'not listening (service not installed yet)' -Result 'INFO'
    }
}
else {
    Add-Row -Group 'Info' -Target ('port ' + $Port) -Check 'host to guest' -Expected 'information only' -Measured 'guest reported no usable IPv4' -Result 'INFO'
}

# ---- Overall verdict ----
$extra = [ordered]@{
    MeasuredAt = $measuredAt; LanTargets = $lanList; InternetTargets = $inetList
    GuestIPv4 = @($guest.GuestIPv4); GuestGateway = $guest.GuestGateway; HandsListeningInGuest = $guest.HandsListening
}
$fails = @($script:Rows | Where-Object { $_.Result -eq 'FAIL' })
if ($lanLeak) {
    Complete-Run -Verdict 'ISOLATION FAILED' -Code 1 -Reason 'the guest got an answer from a LAN target (see FAIL rows). Do not use the workbench until Set-WorkbenchNetworkIsolation.ps1 -Action Show / Apply is fixed.' -Extra $extra
}
if (-not $internetOk -or @($guest.GuestIPv4 | Where-Object { $_ -notlike '169.254.*' }).Count -eq 0) {
    Complete-Run -Verdict 'NOT MEASURED' -Code 2 -Reason 'the guest has no working internet or no IPv4 address, so "blocked" results prove nothing (positive control failed). Fix the guest network and repeat.' -Extra $extra
}
if ($fails.Count -gt 0) {
    Complete-Run -Verdict 'ISOLATION FAILED' -Code 1 -Reason ($fails.Count.ToString() + ' row(s) failed (see FAIL rows).') -Extra $extra
}
Complete-Run -Verdict 'ISOLATION VERIFIED' -Code 0 -Reason ('all rows PASS: ' + $lanList.Count + ' LAN target(s) gave no answer, internet and DNS work, host ACL baseline present, at ' + $measuredAt + '.') -Extra $extra
