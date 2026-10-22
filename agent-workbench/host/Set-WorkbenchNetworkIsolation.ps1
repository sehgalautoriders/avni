#Requires -Version 5.1
<#
.SYNOPSIS
Applies, shows, removes and connects the host-enforced network isolation of the AVNI Agent Workbench VM (Hyper-V Extended Port ACLs).

.DESCRIPTION
The Architect's ruling: the guest must NOT be able to reach other devices on the owner's
LAN or any production system, and this must be enforced ON THE HOST before the guest
ever boots with a connected network. The guest may reach the internet (through the
Default Switch NAT) and may be reached by the host only (the host-side driver talks to
the guest "hands" service on TCP 7001).

Mechanism: Hyper-V Extended Port ACLs on the VM's virtual network adapter
(Add-/Get-/Remove-VMNetworkAdapterExtendedAcl). The virtual switch enforces them on the
host, so nothing done inside the guest, even as guest administrator, can lift them.
Higher -Weight wins.

Actions (-Action):
  Show        Default. Read-only. Prints the measured ACL table and whether it matches
              the inventory this script keeps of the rules it created.
  Apply       Measures the VM, the adapter, the Default Switch gateway and the host's own
              LAN addresses, then (re)creates the rules below. REFUSES, changing nothing,
              if the VM is not Off and its adapter is connected: isolation must be
              applied before a connected boot (run -Action Disconnect or shut the VM
              down first). Reads the rules back and writes an inventory and a report.
  Remove      Needs -Force. Removes ONLY the rules listed in the inventory. Refuses while
              the adapter is connected and the VM is not Off.
  Connect     Connects the adapter to 'Default Switch' ONLY if the read-back shows the
              full inventory and the deny rules present, and the Default Switch gateway
              address is still the one the rules were built for. Otherwise it refuses
              ("isolation not applied, refusing to connect").
  Disconnect  Disconnect-VMNetworkAdapter. Safe at any time.

Rules created by Apply (Direction, Action, Weight, remote):
  Outbound Allow 100  gateway /32, UDP remote port 53, stateful (DNS)
  Outbound Allow  99  gateway /32, TCP remote port 53, stateful (DNS)
  Outbound Allow  98  gateway /32, TCP local port 7001 (replies of the hands service to the host)
                      Nothing else from the guest to the host: the host's own services (SMB 445,
                      RDP 3389, dev servers, databases) stay unreachable from the guest. NAT
                      traffic to the internet carries the public destination address, so it
                      needs no gateway rule.
  Inbound  Allow 101  gateway /32, TCP, local port 7001, stateful (the hands service)
  Inbound  Allow  90  gateway /32, any protocol. Integration services (heartbeat,
                      Copy-VMFile) do NOT use the network, so this is a convenience for the
                      host only, not something the integration services need.
  Outbound Deny   60, 61, ...  each of the host's own LAN IPv4 /32 addresses (belt and braces),
                      each host LAN subnet not already covered by the ranges below, each -ExtraDeny
  Outbound Allow  80, 81, ...  each -ExtraAllow, stateful
  Outbound Deny   50..54  10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16, 100.64.0.0/10
  Inbound  Deny   40..44  the same five ranges (nothing on the LAN or other VMs can reach the guest;
                      the weight 101/90 gateway allows win over this)
  Outbound Deny   55, 56 / Inbound Deny 45, 46  fc00::/7 and fe80::/10 (IPv6 ULA and link-local
                      cannot bypass the IPv4 rules; global IPv6 is left untouched)
  Every rule has its own weight: Hyper-V rejects two rules with the same Direction and Weight on a
  live switch port (0x800700B7, measured 21-10-2026), which is what broke the first Connect.
There is NO catch-all rule: everything not matched, that is the public internet, stays
allowed. That is the intended "approved internet access".

Inventory: ACL rules have no description field, so the rules this script created are listed in
%LOCALAPPDATA%\Avni\workbench\isolation-<vm name, sanitised>.json. Remove and a repeated Apply
remove exactly those rules (matched on Direction, Action, RemoteIPAddress, Weight) and never any
rule that is not in the inventory. If a foreign rule shares a Direction and Weight with one of
ours the script refuses, because removal cannot pick between them.

Every status is MEASURED with its method and time, or NOT MEASURED. A cmdlet returning without
error is never reported as success; the read-back from Get-VMNetworkAdapterExtendedAcl is.
A JSON report is written to %LOCALAPPDATA%\Avni\reports\workbench-isolation-dd-MM-yyyy_HHmm.json.

What it never does:
  - It never starts or stops the VM, never touches another VM, never changes the host firewall.
  - It never deletes anything except the ACL rules listed in its own inventory.
  - It never touches AutoHotkey or any other program on the host.
  - It contains no secrets and makes no internet connections.

Needs an elevated PowerShell (or membership of the "Hyper-V Administrators" group).

Exit code: 0 = done (or nothing to do), 1 = refused or failed.

.PARAMETER Name
VM name. Default 'AVNI Agent Workbench 01'.

.PARAMETER Action
One of Apply, Show, Remove, Connect, Disconnect. Default Show.

.PARAMETER ExtraAllow
Extra IPv4 addresses or CIDR ranges the guest may reach (Outbound Allow, weight 70, stateful).
Refused if a range contains one of the host's own LAN addresses or overlaps an -ExtraDeny.
Ranges inside the private LAN ranges punch a hole in the isolation on purpose; a WARNING is printed.

.PARAMETER ExtraDeny
Extra IPv4 addresses or CIDR ranges to block (Outbound Deny, weight 60), for example a production server.

.PARAMETER Force
Required for -Action Remove.

.EXAMPLE
.\Set-WorkbenchNetworkIsolation.ps1 -Action Apply -ExtraDeny 203.0.113.10

.EXAMPLE
.\Set-WorkbenchNetworkIsolation.ps1 -Action Apply -WhatIf

.EXAMPLE
.\Set-WorkbenchNetworkIsolation.ps1 -Action Connect
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [ValidatePattern('^[A-Za-z0-9]([A-Za-z0-9 ._-]{0,62}[A-Za-z0-9])?$')]
    [string]$Name = 'AVNI Agent Workbench 01',

    [ValidateSet('Apply', 'Show', 'Remove', 'Connect', 'Disconnect')]
    [string]$Action = 'Show',

    [string[]]$ExtraAllow = @(),

    [string[]]$ExtraDeny = @(),

    [switch]$Force
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Script-wide state and constants
# ---------------------------------------------------------------------------
$script:ToolName      = 'Set-WorkbenchNetworkIsolation'
$script:ToolVersion   = '1.1.0'
$script:Culture       = [System.Globalization.CultureInfo]::InvariantCulture
$script:Started       = Get-Date
$script:ExitCode      = 0
$script:Result        = 'unknown'
$script:Detail        = ''
$script:Records       = New-Object System.Collections.Generic.List[object]
$script:Warnings      = New-Object System.Collections.Generic.List[string]
$script:ReportExtra   = [ordered]@{}
$script:SkipReport    = $false
$script:Cmdlet        = $PSCmdlet
$script:SwitchName    = 'Default Switch'
$script:GatewayAlias  = 'vEthernet (Default Switch)'
$script:HandsPort     = 7001
$script:PrivateV4     = @('10.0.0.0/8', '172.16.0.0/12', '192.168.0.0/16', '169.254.0.0/16', '100.64.0.0/10')
$script:PrivateV6     = @('fc00::/7', 'fe80::/10')
# Hyper-V rejects a second Extended ACL rule with the same Direction and Weight once the adapter sits on a
# live switch port (measured 21-10-2026 on the PC: "Cannot create a file when that file already exists",
# 0x800700B7; the same rules were accepted while the adapter was disconnected and then broke Connect).
# So every rule gets its own weight. Higher wins; the bands keep the intended order.
$script:DenyOutWeight = 50       # 50, 51, ... one per private range, Outbound
$script:DenyInWeight  = 40       # 40, 41, ... one per private range, Inbound
$script:HostDenyWeight = 60      # 60, 61, ... host addresses, host subnets, -ExtraDeny (Outbound)
$script:ExtraAllowWeight = 80    # 80, 81, ... -ExtraAllow (Outbound)
$script:GwAnyInWeight = 90
$script:GwWeight      = 100      # Outbound 100/99/98 for DNS and hands replies; Inbound 101 for the hands service

# ---------------------------------------------------------------------------
# Helpers (house style, same as Workbench.ps1)
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
    $script:Records.Add($record)
    if ($mxStatus -eq 'MEASURED') { Write-Tagged -Tag '[MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color $Color }
    else { Write-Tagged -Tag '[NOT MEASURED]' -Text ($What + ': ' + $mxDisplay) -Color 'Yellow' }
    Write-Detail ('method: ' + $Method + ' | at ' + $mxAt)
    return $record
}

function Set-Outcome {
    param([string]$Result, [string]$Detail = '', [int]$Code = 0)
    $script:Result = $Result
    $script:Detail = $Detail
    $script:ExitCode = $Code
}

function Stop-Refused {
    # Nothing was changed. Sets the outcome; the caller returns.
    param([string]$Reason)
    Write-Host ''
    Write-Host ('REFUSED: ' + $Reason) -ForegroundColor Red
    Write-Host 'Nothing was changed.' -ForegroundColor Red
    Set-Outcome -Result 'refused' -Detail (Limit-Text -Text $Reason -Max 400) -Code 1
}

function Get-ReportDirectory {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
    return (Join-Path -Path $env:LOCALAPPDATA -ChildPath 'Avni\reports')
}

function Get-InventoryPath {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
    $safe = ($Name -replace '[^A-Za-z0-9._-]', '_')
    return (Join-Path -Path $env:LOCALAPPDATA -ChildPath ('Avni\workbench\isolation-' + $safe + '.json'))
}

function Save-Report {
    # One JSON report per run (not for -WhatIf). A failure to write is printed and never changes the result.
    if ($script:SkipReport) { return }
    try {
        $dir = Get-ReportDirectory
        [System.IO.Directory]::CreateDirectory($dir) | Out-Null
        $stamp = $script:Started.ToString('dd-MM-yyyy_HHmm', $script:Culture)
        $base = Join-Path -Path $dir -ChildPath ('workbench-isolation-' + $stamp)
        $candidate = $base
        $n = 1
        while (Test-Path -LiteralPath ($candidate + '.json')) { $n++; $candidate = $base + '-' + $n }
        $record = [ordered]@{
            Tool        = $script:ToolName
            ToolVersion = $script:ToolVersion
            Action      = $Action
            Vm          = $Name
            StartedAt   = $script:Started.ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            FinishedAt  = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            Result      = $script:Result
            Detail      = (Limit-Text -Text $script:Detail -Max 500)
            ExitCode    = $script:ExitCode
            Measurements = $script:Records.ToArray()
            Warnings    = $script:Warnings.ToArray()
            Extra       = $script:ReportExtra
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

# ---------------------------------------------------------------------------
# IPv4 / rule-key helpers
# ---------------------------------------------------------------------------
function ConvertTo-Ipv4Long {
    param([string]$Ip)
    $o = $Ip.Split('.')
    return (([long]$o[0] * 16777216) + ([long]$o[1] * 65536) + ([long]$o[2] * 256) + [long]$o[3])
}

function ConvertFrom-Ipv4Long {
    param([long]$Value)
    $a = [math]::Floor($Value / 16777216)
    $b = [math]::Floor(($Value % 16777216) / 65536)
    $c = [math]::Floor(($Value % 65536) / 256)
    $d = $Value % 256
    return ('{0}.{1}.{2}.{3}' -f [int]$a, [int]$b, [int]$c, [int]$d)
}

function Get-Ipv4Range {
    # Parses 'a.b.c.d' or 'a.b.c.d/p'. Returns Start, End, Prefix and a normalised network CIDR. Throws if invalid.
    param([string]$Text)
    $t = ([string]$Text).Trim()
    $m = [regex]::Match($t, '^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})(?:/(\d{1,2}))?$')
    if (-not $m.Success) { throw ("'" + $t + "' is not an IPv4 address or IPv4 CIDR range") }
    foreach ($i in 1..4) {
        if ([int]$m.Groups[$i].Value -gt 255) { throw ("'" + $t + "' has an octet above 255") }
    }
    $prefix = 32
    if ($m.Groups[5].Success) { $prefix = [int]$m.Groups[5].Value }
    if ($prefix -gt 32) { throw ("'" + $t + "' has a prefix length above 32") }
    $addr = ConvertTo-Ipv4Long -Ip ($m.Groups[1].Value + '.' + $m.Groups[2].Value + '.' + $m.Groups[3].Value + '.' + $m.Groups[4].Value)
    $size = [long][math]::Pow(2, 32 - $prefix)
    $start = [long][math]::Floor($addr / $size) * $size
    $end = $start + $size - 1
    return [pscustomobject]([ordered]@{
        Start = $start; End = $end; Prefix = $prefix
        Cidr = ((ConvertFrom-Ipv4Long -Value $start) + '/' + $prefix)
    })
}

function Test-RangeContains {
    param($Outer, $Inner)
    return ($Outer.Start -le $Inner.Start -and $Inner.End -le $Outer.End)
}

function Test-RangeOverlap {
    param($A, $B)
    return ($A.Start -le $B.End -and $B.Start -le $A.End)
}

function Get-RemoteKey {
    # Normalises a RemoteIPAddress value so that what we asked for and what Hyper-V reports compare equal.
    param([string]$Text)
    $t = ([string]$Text).Trim().ToLowerInvariant()
    if ($t -eq '' -or $t -eq '*' -or $t -eq 'any') { return '*' }
    if ($t -match '^\d{1,3}(\.\d{1,3}){3}(/\d{1,2})?$') {
        try {
            $r = Get-Ipv4Range -Text $t
            if ($r.Prefix -eq 32) { return (ConvertFrom-Ipv4Long -Value $r.Start) }
            return $r.Cidr
        }
        catch { return $t }
    }
    if ($t.Contains(':')) {
        $parts = $t.Split('/')
        try {
            $addr = [System.Net.IPAddress]::Parse($parts[0]).ToString().ToLowerInvariant()
            if ($parts.Count -gt 1 -and $parts[1] -ne '128') { return ($addr + '/' + $parts[1]) }
            return $addr
        }
        catch { return $t }
    }
    return $t
}

function Get-RuleKey {
    # The identity we match on: Direction, Action, Weight, RemoteIPAddress.
    param([string]$Direction, [string]$RuleAction, $Weight, [string]$Remote)
    return ('{0}|{1}|{2}|{3}' -f $Direction.ToLowerInvariant(), $RuleAction.ToLowerInvariant(), [int]$Weight, (Get-RemoteKey -Text $Remote))
}

function New-RuleSpec {
    param(
        [string]$Direction, [string]$RuleAction, [int]$Weight, [string]$Remote, [string]$Purpose,
        [string]$Protocol = '', [string]$LocalPort = '', [string]$RemotePort = '', [bool]$Stateful = $false
    )
    return [pscustomobject]([ordered]@{
        Direction = $Direction; Action = $RuleAction; Weight = $Weight; RemoteIPAddress = $Remote
        Protocol = $Protocol; LocalPort = $LocalPort; RemotePort = $RemotePort; Stateful = $Stateful; Purpose = $Purpose
        Key = (Get-RuleKey -Direction $Direction -RuleAction $RuleAction -Weight $Weight -Remote $Remote)
    })
}

# ---------------------------------------------------------------------------
# Hyper-V and host lookups
# ---------------------------------------------------------------------------
function Get-TargetVm {
    $found = @(Get-VM -Name $Name -ErrorAction SilentlyContinue)
    if ($found.Count -eq 0) { throw ("no VM named '" + $Name + "' (Get-VM found none)") }
    return $found[0]
}

function Get-TargetAdapter {
    $a = @(Get-VMNetworkAdapter -VMName $Name)
    if ($a.Count -ne 1) { throw ("expected exactly 1 network adapter on '" + $Name + "', Get-VMNetworkAdapter returned " + $a.Count) }
    return $a[0]
}

function Get-AdapterInfo {
    $ad = Get-TargetAdapter
    $c = Get-PropValue -Object $ad -Name 'Connected'
    if ($null -eq $c) { throw 'Hyper-V reported no Connected value for the adapter' }
    return [pscustomobject]([ordered]@{ AdapterName = [string]$ad.Name; SwitchName = [string]$ad.SwitchName; Connected = [bool]$c })
}

function Get-GatewayInfo {
    # The Default Switch gateway as seen from the host = the host's own address on vEthernet (Default Switch).
    $ips = @(Get-NetIPAddress -InterfaceAlias $script:GatewayAlias -AddressFamily IPv4 -ErrorAction Stop |
        Where-Object { $_.IPAddress -notlike '169.254.*' })
    if ($ips.Count -eq 0) { throw ("'" + $script:GatewayAlias + "' has no usable IPv4 address") }
    $first = $ips[0]
    $range = Get-Ipv4Range -Text ([string]$first.IPAddress + '/' + [string]$first.PrefixLength)
    return [pscustomobject]([ordered]@{ Ip = [string]$first.IPAddress; PrefixLength = [int]$first.PrefixLength; Subnet = $range.Cidr })
}

function Get-HostLanAddresses {
    # The host's own LAN IPv4 addresses: DHCP or manual, not vEthernet, not loopback.
    $all = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop | Where-Object {
        ([string]$_.PrefixOrigin -in @('Dhcp', 'Manual')) -and
        ([string]$_.InterfaceAlias -notlike 'vEthernet*') -and
        ([string]$_.InterfaceAlias -notlike '*Loopback*') -and
        ([string]$_.IPAddress -notlike '127.*')
    })
    $seen = @{}
    $list = New-Object System.Collections.Generic.List[object]
    foreach ($a in $all) {
        $ip = [string]$a.IPAddress
        if ($seen.ContainsKey($ip)) { continue }
        $seen[$ip] = $true
        $list.Add([pscustomobject]([ordered]@{ Ip = $ip; PrefixLength = [int]$a.PrefixLength; Alias = [string]$a.InterfaceAlias }))
    }
    return $list.ToArray()
}

function Get-VmContext {
    # Step 1 of Apply, also used by the other actions: measure and print what the decision depends on.
    param([switch]$NeedHostNetwork)
    $ctx = [ordered]@{ Vm = $null; Adapter = $null; Gateway = $null; Lan = $null }
    $ctx.Vm = Invoke-Measure -What 'VM exists and its State' -Method 'Get-VM -Name (State)' -Probe {
        $s = [string](Get-TargetVm).State
        New-MeasureResult -Value $s -Display ("'" + $Name + "' exists, State = " + $s)
    }
    $ctx.Adapter = Invoke-Measure -What 'Adapter SwitchName and Connected' -Method 'Get-VMNetworkAdapter (SwitchName, Connected)' -Probe {
        $info = Get-AdapterInfo
        $sw = $info.SwitchName
        if ([string]::IsNullOrEmpty($sw)) { $sw = '(none)' }
        New-MeasureResult -Value $info -Display ("adapter '{0}', SwitchName = {1}, Connected = {2}" -f $info.AdapterName, $sw, $info.Connected)
    }
    if ($NeedHostNetwork) {
        $ctx.Gateway = Invoke-Measure -What ("Default Switch gateway (host address on '" + $script:GatewayAlias + "')") -Method 'Get-NetIPAddress -InterfaceAlias -AddressFamily IPv4 (IPAddress, PrefixLength)' -Probe {
            $g = Get-GatewayInfo
            New-MeasureResult -Value $g -Display ("{0}/{1} (subnet {2})" -f $g.Ip, $g.PrefixLength, $g.Subnet)
        }
        $ctx.Lan = Invoke-Measure -What "Host's own LAN IPv4 addresses (the guest must NOT reach these)" -Method 'Get-NetIPAddress -AddressFamily IPv4 (PrefixOrigin Dhcp or Manual; vEthernet and loopback excluded)' -Probe {
            $l = @(Get-HostLanAddresses)
            if ($l.Count -eq 0) { return (New-MeasureResult -Value @() -Display 'none found') }
            New-MeasureResult -Value $l -Display ((@($l | ForEach-Object { $_.Ip + '/' + $_.PrefixLength + ' (' + $_.Alias + ')' })) -join ', ')
        }
    }
    return [pscustomobject]$ctx
}

# ---------------------------------------------------------------------------
# ACL read-back, table, inventory
# ---------------------------------------------------------------------------
function Get-AclReadBack {
    $raw = @(Get-VMNetworkAdapterExtendedAcl -VMName $Name -ErrorAction Stop)
    $list = New-Object System.Collections.Generic.List[object]
    foreach ($r in $raw) {
        $dir = [string](Get-PropValue -Object $r -Name 'Direction')
        $act = [string](Get-PropValue -Object $r -Name 'Action')
        $wt = Get-PropValue -Object $r -Name 'Weight'
        $rem = [string](Get-PropValue -Object $r -Name 'RemoteIPAddress')
        $sf = Get-PropValue -Object $r -Name 'Stateful'
        $list.Add([pscustomobject]([ordered]@{
            Direction = $dir; Action = $act; RemoteIPAddress = $rem; Weight = [int]$wt
            Stateful = [bool]$sf
            Protocol = [string](Get-PropValue -Object $r -Name 'Protocol')
            LocalPort = [string](Get-PropValue -Object $r -Name 'LocalPort')
            RemotePort = [string](Get-PropValue -Object $r -Name 'RemotePort')
            Key = (Get-RuleKey -Direction $dir -RuleAction $act -Weight $wt -Remote $rem)
        }))
    }
    return $list.ToArray()
}

function Show-AclTable {
    param($Rules)
    $rows = @($Rules)
    if ($rows.Count -eq 0) {
        Write-Host '  (no Extended ACL rules on this VM)'
        return
    }
    $sorted = $rows | Sort-Object -Property @{ Expression = 'Direction'; Descending = $false }, @{ Expression = 'Weight'; Descending = $true }, 'RemoteIPAddress'
    $text = ($sorted | Select-Object Direction, Action, RemoteIPAddress, Weight, Stateful, Protocol, LocalPort | Format-Table -AutoSize | Out-String)
    foreach ($line in ($text -split "`r?`n")) {
        if ($line.Trim().Length -gt 0) { Write-Host ('  ' + $line) }
    }
}

function Read-Inventory {
    # Returns $null when there is no inventory file. Throws if the file exists but cannot be used.
    $path = Get-InventoryPath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { return $null }
    $inv = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($null -eq (Get-PropValue -Object $inv -Name 'Rules')) { throw ("inventory file has no Rules list: " + $path) }
    return $inv
}

function Get-InventoryRuleList {
    param($Inventory)
    $list = New-Object System.Collections.Generic.List[object]
    foreach ($r in @($Inventory.Rules)) {
        $list.Add([pscustomobject]([ordered]@{
            Direction = [string]$r.Direction; Action = [string]$r.Action; Weight = [int]$r.Weight
            RemoteIPAddress = [string]$r.RemoteIPAddress
            Key = (Get-RuleKey -Direction ([string]$r.Direction) -RuleAction ([string]$r.Action) -Weight $r.Weight -Remote ([string]$r.RemoteIPAddress))
        }))
    }
    return $list.ToArray()
}

function Write-Inventory {
    param($Plan, $Gateway, $HostLan, [string]$State)
    $path = Get-InventoryPath
    [System.IO.Directory]::CreateDirectory((Split-Path -Parent $path)) | Out-Null
    $inv = [ordered]@{
        Tool        = $script:ToolName
        ToolVersion = $script:ToolVersion
        Vm          = $Name
        State       = $State
        WrittenAt   = (Get-Date).ToString('dd-MM-yyyy HH:mm:ss', $script:Culture)
        GatewayIp   = $Gateway.Ip
        GatewayPrefixLength = $Gateway.PrefixLength
        HostLanAddresses = @($HostLan | ForEach-Object { $_.Ip + '/' + $_.PrefixLength })
        ExtraAllow  = @($ExtraAllow)
        ExtraDeny   = @($ExtraDeny)
        Rules       = @($Plan | ForEach-Object {
            [ordered]@{
                Direction = $_.Direction; Action = $_.Action; Weight = $_.Weight; RemoteIPAddress = $_.RemoteIPAddress
                Protocol = $_.Protocol; LocalPort = $_.LocalPort; RemotePort = $_.RemotePort; Stateful = $_.Stateful; Purpose = $_.Purpose
            }
        })
    }
    $json = ConvertTo-Json -InputObject $inv -Depth 6
    [System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))
    return $path
}

function Get-BaselineDenyKeys {
    # The deny rules whose presence Connect insists on.
    $keys = New-Object System.Collections.Generic.List[string]
    $i = 0
    foreach ($r in @($script:PrivateV4 + $script:PrivateV6)) {
        $keys.Add((Get-RuleKey -Direction 'Outbound' -RuleAction 'Deny' -Weight ($script:DenyOutWeight + $i) -Remote $r))
        $keys.Add((Get-RuleKey -Direction 'Inbound' -RuleAction 'Deny' -Weight ($script:DenyInWeight + $i) -Remote $r))
        $i++
    }
    return $keys.ToArray()
}

function Compare-WithInventory {
    param($Inventory, $Present)
    $presentKeys = @{}
    foreach ($p in @($Present)) { $presentKeys[$p.Key] = $true }
    $invRules = @()
    if ($null -ne $Inventory) { $invRules = @(Get-InventoryRuleList -Inventory $Inventory) }
    $invKeys = @{}
    foreach ($r in $invRules) { $invKeys[$r.Key] = $true }
    $missing = @($invRules | Where-Object { -not $presentKeys.ContainsKey($_.Key) })
    $foreign = @(@($Present) | Where-Object { -not $invKeys.ContainsKey($_.Key) })
    $baselineMissing = @(Get-BaselineDenyKeys | Where-Object { -not $presentKeys.ContainsKey($_) })
    return [pscustomobject]([ordered]@{
        HasInventory = ($null -ne $Inventory)
        InventoryCount = $invRules.Count
        PresentOfInventory = ($invRules.Count - $missing.Count)
        Missing = $missing
        Foreign = $foreign
        BaselineMissing = $baselineMissing
    })
}

# ---------------------------------------------------------------------------
# Plan
# ---------------------------------------------------------------------------
function Get-PlannedRules {
    param($Gateway, $HostLan, [string[]]$Allow, [string[]]$Deny)
    $plan = New-Object System.Collections.Generic.List[object]
    $keys = @{}
    $add = {
        param($Spec)
        if (-not $keys.ContainsKey($Spec.Key)) { $keys[$Spec.Key] = $true; $plan.Add($Spec) }
    }
    $gw = $Gateway.Ip + '/32'
    # Guest -> host is allowed ONLY for DNS and for the replies of the hands service. NAT traffic to the
    # internet carries the public destination address, so it never needs a rule for the gateway itself.
    # Distinct weights keep the rule keys distinct (keys do not include protocol or port).
    & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Allow' -Weight $script:GwWeight -Remote $gw -Protocol 'UDP' -RemotePort '53' -Stateful $true -Purpose 'DNS to the Default Switch gateway (UDP 53)')
    & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Allow' -Weight ($script:GwWeight - 1) -Remote $gw -Protocol 'TCP' -RemotePort '53' -Stateful $true -Purpose 'DNS to the Default Switch gateway (TCP 53)')
    & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Allow' -Weight ($script:GwWeight - 2) -Remote $gw -Protocol 'TCP' -LocalPort ([string]$script:HandsPort) -Purpose 'replies of the hands service to the host (guest local port 7001), works even if stateful is not honoured')
    & $add (New-RuleSpec -Direction 'Inbound' -RuleAction 'Allow' -Weight ($script:GwWeight + 1) -Remote $gw -Protocol 'TCP' -LocalPort ([string]$script:HandsPort) -Stateful $true -Purpose 'hands service from the host only')
    & $add (New-RuleSpec -Direction 'Inbound' -RuleAction 'Allow' -Weight $script:GwAnyInWeight -Remote $gw -Purpose 'any protocol from the host gateway (convenience; integration services do not use the network)')
    # One weight per rule (see the weight constants): the same index for a range in both directions,
    # in the same order as Get-BaselineDenyKeys.
    $i = 0
    foreach ($r in @($script:PrivateV4 + $script:PrivateV6)) {
        $why = if ($r -like '*:*') { 'block IPv6 ULA/link-local' } else { 'block private/link-local/CGNAT range' }
        & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Deny' -Weight ($script:DenyOutWeight + $i) -Remote $r -Purpose $why)
        & $add (New-RuleSpec -Direction 'Inbound' -RuleAction 'Deny' -Weight ($script:DenyInWeight + $i) -Remote $r -Purpose $why)
        $i++
    }
    $privRanges = @($script:PrivateV4 | ForEach-Object { Get-Ipv4Range -Text $_ })
    $hw = $script:HostDenyWeight
    foreach ($h in @($HostLan)) {
        & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Deny' -Weight $hw -Remote ($h.Ip + '/32') -Purpose 'host own LAN address'); $hw++
        if ($h.PrefixLength -ge 8 -and $h.PrefixLength -le 30) {
            $net = Get-Ipv4Range -Text ($h.Ip + '/' + $h.PrefixLength)
            $covered = $false
            foreach ($p in $privRanges) { if (Test-RangeContains -Outer $p -Inner $net) { $covered = $true } }
            if (-not $covered) {
                & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Deny' -Weight $hw -Remote $net.Cidr -Purpose 'host LAN subnet (not covered by the private ranges)'); $hw++
            }
        }
    }
    foreach ($d in @($Deny)) {
        $r = Get-Ipv4Range -Text $d
        & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Deny' -Weight $hw -Remote $r.Cidr -Purpose 'ExtraDeny'); $hw++
    }
    if ($hw -gt $script:ExtraAllowWeight) { throw ('too many host/ExtraDeny rules: weights ' + $script:HostDenyWeight + '..' + ($hw - 1) + ' run into the ExtraAllow band at ' + $script:ExtraAllowWeight) }
    $aw = $script:ExtraAllowWeight
    foreach ($a in @($Allow)) {
        $r = Get-Ipv4Range -Text $a
        & $add (New-RuleSpec -Direction 'Outbound' -RuleAction 'Allow' -Weight $aw -Remote $r.Cidr -Stateful $true -Purpose 'ExtraAllow'); $aw++
    }
    if ($aw -gt $script:GwAnyInWeight) { throw ('too many ExtraAllow rules: weights run into the gateway band at ' + $script:GwAnyInWeight) }
    # Hyper-V needs Direction + Weight to be unique on the adapter: prove it here, never on the switch port.
    $dup = @($plan | Group-Object -Property { $_.Direction + '|' + $_.Weight } | Where-Object { $_.Count -gt 1 })
    if ($dup.Count -gt 0) { throw ('rule plan has duplicate Direction+Weight: ' + (($dup | ForEach-Object { $_.Name }) -join ', ')) }
    return $plan.ToArray()
}

function Test-PlanInputs {
    # Returns a list of refusal reasons (empty = fine). Prints WARNINGs for deliberate holes.
    param($Gateway, $HostLan, [string[]]$Allow, [string[]]$Deny)
    $errors = New-Object System.Collections.Generic.List[string]
    $allowR = New-Object System.Collections.Generic.List[object]
    $denyR = New-Object System.Collections.Generic.List[object]
    foreach ($a in @($Allow)) {
        try { $allowR.Add((Get-Ipv4Range -Text $a)) } catch { $errors.Add('-ExtraAllow: ' + $_.Exception.Message) }
    }
    foreach ($d in @($Deny)) {
        try { $denyR.Add((Get-Ipv4Range -Text $d)) } catch { $errors.Add('-ExtraDeny: ' + $_.Exception.Message) }
    }
    if ($errors.Count -gt 0) { return $errors.ToArray() }
    $privRanges = @($script:PrivateV4 | ForEach-Object { Get-Ipv4Range -Text $_ })
    foreach ($a in $allowR) {
        foreach ($h in @($HostLan)) {
            $hr = Get-Ipv4Range -Text ($h.Ip + '/32')
            if (Test-RangeOverlap -A $a -B $hr) { $errors.Add('-ExtraAllow ' + $a.Cidr + ' contains the host LAN address ' + $h.Ip + '; the guest must not reach the host through its LAN address') }
        }
        foreach ($d in $denyR) {
            if (Test-RangeOverlap -A $a -B $d) { $errors.Add('-ExtraAllow ' + $a.Cidr + ' overlaps -ExtraDeny ' + $d.Cidr + '; a deny must win, so give one or the other') }
        }
        foreach ($p in $privRanges) {
            if (Test-RangeOverlap -A $a -B $p) {
                Add-Warning ('-ExtraAllow ' + $a.Cidr + ' is inside or touches the private range ' + $p.Cidr + ': this deliberately punches a hole in the LAN isolation (Allow 70 beats Deny 50).')
            }
        }
    }
    return $errors.ToArray()
}

function Show-Plan {
    param($Plan)
    Write-Host ''
    Write-Host ('PLANNED RULES (' + @($Plan).Count + '); no catch-all rule: everything not matched, i.e. the public internet, stays ALLOWED (approved internet access):') -ForegroundColor White
    $text = (@($Plan) | Sort-Object -Property @{ Expression = 'Direction'; Descending = $false }, @{ Expression = 'Weight'; Descending = $true } |
        Select-Object Direction, Action, RemoteIPAddress, Weight, Stateful, Protocol, LocalPort, Purpose | Format-Table -AutoSize | Out-String)
    foreach ($line in ($text -split "`r?`n")) { if ($line.Trim().Length -gt 0) { Write-Host ('  ' + $line) } }
}

# ---------------------------------------------------------------------------
# Rule add / remove
# ---------------------------------------------------------------------------
function Add-AclRule {
    # Adds one rule. If a stateful rule is rejected it is retried non-stateful (never more permissive) and flagged.
    param($Spec)
    $p = @{
        VMName = $Name; Action = $Spec.Action; Direction = $Spec.Direction; Weight = $Spec.Weight
        RemoteIPAddress = $Spec.RemoteIPAddress; ErrorAction = 'Stop'
    }
    if ($Spec.Protocol) { $p['Protocol'] = $Spec.Protocol }
    if ($Spec.LocalPort) { $p['LocalPort'] = $Spec.LocalPort }
    if ($Spec.RemotePort) { $p['RemotePort'] = $Spec.RemotePort }
    if ($Spec.Stateful) {
        try {
            Add-VMNetworkAdapterExtendedAcl @p -Stateful $true
            return [pscustomobject]@{ Spec = $Spec; StatefulApplied = $true; Note = '' }
        }
        catch {
            $first = Limit-Text -Text $_.Exception.Message -Max 200
            Add-VMNetworkAdapterExtendedAcl @p
            Add-Warning ('Rule ' + $Spec.Direction + ' ' + $Spec.Action + ' ' + $Spec.RemoteIPAddress + ' w' + $Spec.Weight + ' was rejected as stateful (' + $first + ') and added NON-stateful instead.')
            return [pscustomobject]@{ Spec = $Spec; StatefulApplied = $false; Note = ('stateful rejected: ' + $first) }
        }
    }
    Add-VMNetworkAdapterExtendedAcl @p
    return [pscustomobject]@{ Spec = $Spec; StatefulApplied = $false; Note = '' }
}

function Remove-OurRules {
    # Removes exactly the inventoried rules. Returns an object with Refused/Reason or the removal result.
    param($Inventory)
    $inv = @(Get-InventoryRuleList -Inventory $Inventory)
    $ours = @{}
    foreach ($r in $inv) { $ours[$r.Key] = $true }
    $present = @(Get-AclReadBack)
    $groups = @($inv | ForEach-Object { [string]$_.Direction + '|' + [string]$_.Weight } | Sort-Object -Unique)
    # Pre-check: nothing is removed if any group also holds a rule that is not ours.
    foreach ($g in $groups) {
        $dir = $g.Split('|')[0]; $wt = [int]$g.Split('|')[1]
        $inGroup = @($present | Where-Object { $_.Direction -eq $dir -and $_.Weight -eq $wt })
        $foreign = @($inGroup | Where-Object { -not $ours.ContainsKey($_.Key) })
        if ($foreign.Count -gt 0) {
            $names = (@($foreign | ForEach-Object { $_.Direction + ' ' + $_.Action + ' ' + $_.RemoteIPAddress + ' w' + $_.Weight }) -join '; ')
            return [pscustomobject]@{ Refused = $true; Reason = ("rule(s) that this script did not create share a Direction and Weight with ours (" + $names + "); removal cannot pick between them. Remove or re-weight them by hand, then repeat."); RemovedCount = 0; LeftOver = @() }
        }
    }
    $before = @($present | Where-Object { $ours.ContainsKey($_.Key) })
    foreach ($g in $groups) {
        $dir = $g.Split('|')[0]; $wt = [int]$g.Split('|')[1]
        $tries = 0
        $limit = @($inv | Where-Object { [string]$_.Direction -eq $dir -and [int]$_.Weight -eq $wt }).Count + 1
        while ($tries -lt $limit) {
            $tries++
            $now = @(Get-AclReadBack | Where-Object { $_.Direction -eq $dir -and $_.Weight -eq $wt -and $ours.ContainsKey($_.Key) })
            if ($now.Count -eq 0) { break }
            Remove-VMNetworkAdapterExtendedAcl -VMName $Name -Direction $dir -Weight $wt -ErrorAction Stop
        }
    }
    $after = @(Get-AclReadBack)
    $left = @($after | Where-Object { $ours.ContainsKey($_.Key) })
    return [pscustomobject]@{ Refused = $false; Reason = ''; RemovedCount = ($before.Count - $left.Count); LeftOver = $left }
}

# ---------------------------------------------------------------------------
# Actions
# ---------------------------------------------------------------------------
function Show-InventoryComparison {
    param($Cmp)
    if (-not $Cmp.HasInventory) {
        Write-Tagged -Tag '[MEASURED]' -Text 'Inventory: none found (this script has not applied rules for this VM, or the file was removed)' -Color 'Yellow'
    }
    else {
        $line = 'Inventory: {0} rules listed, {1} present on the adapter, {2} missing' -f $Cmp.InventoryCount, $Cmp.PresentOfInventory, @($Cmp.Missing).Count
        if (@($Cmp.Missing).Count -eq 0) { Write-Tagged -Tag '[MEASURED]' -Text ($line + ': inventory MATCHES the adapter') -Color 'Cyan' }
        else { Write-Tagged -Tag '[MEASURED]' -Text ($line + ': DRIFT') -Color 'Red' }
        foreach ($m in @($Cmp.Missing)) { Write-Detail ('missing: ' + $m.Direction + ' ' + $m.Action + ' ' + $m.RemoteIPAddress + ' w' + $m.Weight) }
    }
    if (@($Cmp.Foreign).Count -gt 0) {
        Write-Tagged -Tag '[MEASURED]' -Text (@($Cmp.Foreign).Count.ToString() + ' rule(s) on the adapter are NOT in the inventory (not created by this script; never touched by it)') -Color 'Yellow'
    }
    if (@($Cmp.BaselineMissing).Count -eq 0) { Write-Tagged -Tag '[MEASURED]' -Text 'Baseline deny rules (5 IPv4 ranges + fc00::/7 + fe80::/10, both directions, one weight each): all present' -Color 'Cyan' }
    else { Write-Tagged -Tag '[MEASURED]' -Text ('Baseline deny rules: ' + @($Cmp.BaselineMissing).Count + ' MISSING') -Color 'Red' }
}

function Invoke-ShowAction {
    $ctx = Get-VmContext
    $null = Get-TargetVm
    $present = $null
    $rec = Invoke-Measure -What 'Extended ACL rules on the VM' -Method 'Get-VMNetworkAdapterExtendedAcl -VMName' -Probe {
        $r = @(Get-AclReadBack)
        New-MeasureResult -Value $r -Display ($r.Count.ToString() + ' rule(s)')
    }
    if ($rec.Status -ne 'MEASURED') { Set-Outcome -Result 'show-not-measured' -Detail 'could not read the ACLs' -Code 1; return }
    $present = @($rec.Value)
    Show-AclTable -Rules $present
    $inv = $null
    try { $inv = Read-Inventory } catch { Add-Warning ('Inventory unreadable: ' + (Limit-Text -Text $_.Exception.Message)) }
    $cmp = Compare-WithInventory -Inventory $inv -Present $present
    Show-InventoryComparison -Cmp $cmp
    Write-Host ''
    Write-Host 'Reminder: there is no catch-all rule; the public internet stays allowed on purpose.'
    $script:ReportExtra['Rules'] = $present
    $script:ReportExtra['InventoryMatches'] = ($cmp.HasInventory -and @($cmp.Missing).Count -eq 0)
    $script:ReportExtra['BaselineDenyMissing'] = @($cmp.BaselineMissing)
    Set-Outcome -Result 'shown' -Detail ('rules ' + $present.Count + '; inventory matches: ' + $script:ReportExtra['InventoryMatches'])
}

function Invoke-ApplyAction {
    $ctx = Get-VmContext -NeedHostNetwork
    foreach ($k in @('Vm', 'Adapter', 'Gateway', 'Lan')) {
        if ($ctx.$k.Status -ne 'MEASURED') { Stop-Refused ('"' + $ctx.$k.What + '" could not be measured, so the rules cannot be built safely.'); return }
    }
    $state = [string]$ctx.Vm.Value
    $adapter = $ctx.Adapter.Value
    $gateway = $ctx.Gateway.Value
    $hostLan = @($ctx.Lan.Value)

    # Step 2: isolation must exist before a connected boot.
    if ($adapter.Connected -and $state -ne 'Off') {
        Stop-Refused ("the VM is '" + $state + "' and its adapter is connected to '" + $adapter.SwitchName + "'. Isolation must be applied before a connected boot. Run .\Set-WorkbenchNetworkIsolation.ps1 -Action Disconnect first (or shut the VM down), then Apply, then Connect.")
        return
    }
    if ($hostLan.Count -eq 0) { Add-Warning 'No host LAN IPv4 address was found; only the fixed private ranges are blocked.' }

    $problems = @(Test-PlanInputs -Gateway $gateway -HostLan $hostLan -Allow $ExtraAllow -Deny $ExtraDeny)
    if ($problems.Count -gt 0) { Stop-Refused ($problems -join ' | '); return }
    $plan = @(Get-PlannedRules -Gateway $gateway -HostLan $hostLan -Allow $ExtraAllow -Deny $ExtraDeny)
    Show-Plan -Plan $plan

    $inv = $null
    try { $inv = Read-Inventory } catch { Stop-Refused ('the inventory file is unreadable (' + (Limit-Text -Text $_.Exception.Message) + '). Fix or delete that one file by hand, then repeat.'); return }
    $present = @(Get-AclReadBack)
    $invKeys = @{}
    if ($null -ne $inv) { foreach ($r in @(Get-InventoryRuleList -Inventory $inv)) { $invKeys[$r.Key] = $true } }
    $presentKeys = @{}
    foreach ($pr in $present) { $presentKeys[$pr.Key] = $true }
    $clash = @($plan | Where-Object { $presentKeys.ContainsKey($_.Key) -and -not $invKeys.ContainsKey($_.Key) })
    if ($clash.Count -gt 0) {
        Stop-Refused ('rule(s) identical to ours already exist on the adapter but are not in the inventory, so they cannot be proven to be ours: ' + ((@($clash | ForEach-Object { $_.Direction + ' ' + $_.Action + ' ' + $_.RemoteIPAddress + ' w' + $_.Weight })) -join '; ') + '. Remove them by hand or restore the inventory file.')
        return
    }

    $target = "Extended ACLs on '" + $Name + "' (" + $plan.Count + ' rules)'
    if (-not $script:Cmdlet.ShouldProcess($target, 'Remove the rules this script created earlier, then add the planned rules')) {
        $script:SkipReport = $true
        Write-Host 'WhatIf/Confirm: no changes were made. All measurements above are real.' -ForegroundColor Yellow
        Set-Outcome -Result 'whatif' -Detail 'no changes'
        return
    }

    # Step 3: remove exactly our earlier rules.
    if ($null -ne $inv) {
        Write-Host ''
        Write-Host 'Removing the rules this script created earlier (from the inventory) ...'
        $rm = Remove-OurRules -Inventory $inv
        if ($rm.Refused) { Stop-Refused $rm.Reason; return }
        if (@($rm.LeftOver).Count -gt 0) {
            Stop-Refused ('after removal ' + @($rm.LeftOver).Count + ' of our earlier rules are still present; stopping before adding anything.')
            return
        }
        Write-Tagged -Tag '[MEASURED]' -Text ('Earlier rules removed: ' + $rm.RemovedCount) -Color 'Cyan'
        Write-Detail ('method: Get-VMNetworkAdapterExtendedAcl before/after Remove-VMNetworkAdapterExtendedAcl | at ' + (Get-Stamp))
    }

    # The inventory lists the PLANNED rules before anything is added, so a half-finished Apply can still be cleaned up.
    $invPath = Write-Inventory -Plan $plan -Gateway $gateway -HostLan $hostLan -State 'applying'
    Write-Host ('Inventory written: ' + $invPath)

    # Step 4: add the rules. Stop at the first failure; what exists stays inventoried and the adapter is unconnected-safe.
    Write-Host 'Adding rules ...'
    $results = New-Object System.Collections.Generic.List[object]
    $failed = $null
    foreach ($spec in $plan) {
        try {
            $results.Add((Add-AclRule -Spec $spec))
        }
        catch {
            $failed = ('Add-VMNetworkAdapterExtendedAcl failed for ' + $spec.Direction + ' ' + $spec.Action + ' ' + $spec.RemoteIPAddress + ' w' + $spec.Weight + ': ' + (Limit-Text -Text $_.Exception.Message -Max 250))
            break
        }
    }

    # Step 5: read back. This, not the cmdlets returning, is the evidence.
    Write-Host ''
    $rb = Invoke-Measure -What 'Extended ACL rules after Apply (read-back)' -Method 'Get-VMNetworkAdapterExtendedAcl -VMName' -Probe {
        $r = @(Get-AclReadBack)
        New-MeasureResult -Value $r -Display ($r.Count.ToString() + ' rule(s)')
    }
    if ($rb.Status -ne 'MEASURED') {
        Stop-Refused 'Rules were sent but the read-back failed, so they are NOT confirmed. Do not connect. Run -Action Show.'
        $script:ExitCode = 1
        return
    }
    $after = @($rb.Value)
    Show-AclTable -Rules $after
    $afterKeys = @{}
    foreach ($a in $after) { $afterKeys[$a.Key] = $true }
    $missing = @($plan | Where-Object { -not $afterKeys.ContainsKey($_.Key) })
    $script:ReportExtra['Plan'] = $plan
    $script:ReportExtra['Rules'] = $after
    $script:ReportExtra['StatefulFallbacks'] = @($results | Where-Object { $_.Spec.Stateful -and -not $_.StatefulApplied } | ForEach-Object { $_.Spec.Key })
    $script:ReportExtra['InventoryPath'] = $invPath
    $script:ReportExtra['Gateway'] = $gateway
    $script:ReportExtra['HostLan'] = $hostLan

    if ($null -ne $failed -or $missing.Count -gt 0) {
        $why = $failed
        if ($null -eq $why) { $why = ($missing.Count.ToString() + ' planned rule(s) are missing in the read-back') }
        Write-Host ''
        Write-Host ('FAILED: ' + $why) -ForegroundColor Red
        foreach ($m in $missing) { Write-Detail ('missing: ' + $m.Direction + ' ' + $m.Action + ' ' + $m.RemoteIPAddress + ' w' + $m.Weight) }
        Write-Host 'The rules that do exist are in the inventory and the adapter was not connected by this script. Fix the cause and repeat -Action Apply (it removes our rules first). Do not -Action Connect.' -ForegroundColor Red
        $null = Write-Inventory -Plan $plan -Gateway $gateway -HostLan $hostLan -State 'incomplete'
        Set-Outcome -Result 'apply-incomplete' -Detail (Limit-Text -Text $why -Max 400) -Code 1
        return
    }
    $null = Write-Inventory -Plan $plan -Gateway $gateway -HostLan $hostLan -State 'applied-and-read-back'
    Write-Host ''
    Write-Tagged -Tag '[MEASURED]' -Text ('All ' + $plan.Count + ' planned rules are present in the read-back (matched on Direction, Action, RemoteIPAddress, Weight) at ' + (Get-Stamp)) -Color 'Green'
    Write-Detail 'method: Get-VMNetworkAdapterExtendedAcl compared with the plan'
    Write-Host 'No catch-all rule exists: the public internet stays ALLOWED on purpose (approved internet access).'
    Write-Host 'This proves the rules exist on the adapter. Behaviour is proven by Test-WorkbenchIsolation.ps1 once the guest is up and connected.'
    Write-Host 'Next: .\Set-WorkbenchNetworkIsolation.ps1 -Action Connect'
    Set-Outcome -Result 'applied-read-back-confirmed' -Detail ($plan.Count.ToString() + ' rules confirmed')
}

function Invoke-RemoveAction {
    $ctx = Get-VmContext
    if (-not $Force) {
        Stop-Refused '-Action Remove needs -Force: it removes the host-side isolation of the workbench VM.'
        return
    }
    if ($ctx.Vm.Status -ne 'MEASURED' -or $ctx.Adapter.Status -ne 'MEASURED') { Stop-Refused 'the VM or adapter state could not be measured.'; return }
    if ($ctx.Adapter.Value.Connected -and [string]$ctx.Vm.Value -ne 'Off') {
        Stop-Refused ("the VM is '" + $ctx.Vm.Value + "' and its adapter is connected: removing the rules now would expose the LAN at once. Run -Action Disconnect first.")
        return
    }
    $inv = $null
    try { $inv = Read-Inventory } catch { Stop-Refused ('the inventory file is unreadable (' + (Limit-Text -Text $_.Exception.Message) + ').'); return }
    if ($null -eq $inv) {
        Write-Host 'No inventory exists, so there is nothing this script may remove. Rules on the adapter (if any) are not ours and were left alone:'
        Show-AclTable -Rules @(Get-AclReadBack)
        Set-Outcome -Result 'noop-no-inventory' -Detail 'no inventory'
        return
    }
    if (-not $script:Cmdlet.ShouldProcess("Extended ACLs on '" + $Name + "'", 'Remove the rules listed in the inventory')) {
        $script:SkipReport = $true
        Write-Host 'WhatIf/Confirm: no changes were made.' -ForegroundColor Yellow
        Set-Outcome -Result 'whatif' -Detail 'no changes'
        return
    }
    $rm = Remove-OurRules -Inventory $inv
    if ($rm.Refused) { Stop-Refused $rm.Reason; return }
    $rb = Invoke-Measure -What 'Extended ACL rules after Remove (read-back)' -Method 'Get-VMNetworkAdapterExtendedAcl -VMName' -Probe {
        $r = @(Get-AclReadBack)
        New-MeasureResult -Value $r -Display ($r.Count.ToString() + ' rule(s) remain')
    }
    if ($rb.Status -eq 'MEASURED') { Show-AclTable -Rules @($rb.Value); $script:ReportExtra['RulesAfter'] = $rb.Value }
    if (@($rm.LeftOver).Count -gt 0 -or $rb.Status -ne 'MEASURED') {
        Write-Host ('Some of our rules are still present or the read-back failed (' + @($rm.LeftOver).Count + ' left). The inventory file was kept.') -ForegroundColor Red
        Set-Outcome -Result 'remove-incomplete' -Detail ('left over ' + @($rm.LeftOver).Count) -Code 1
        return
    }
    $invPath = Get-InventoryPath
    $kept = $invPath
    try {
        $archive = $invPath -replace '\.json$', ('-removed-' + (Get-Date).ToString('dd-MM-yyyy_HHmmss', $script:Culture) + '.json')
        [System.IO.File]::Move($invPath, $archive)
        $kept = $archive
    }
    catch { Add-Warning ('Could not rename the inventory (' + (Limit-Text -Text $_.Exception.Message) + '); it still lists rules that no longer exist.') }
    Write-Host ('Removed ' + $rm.RemovedCount + ' rule(s). Inventory kept as: ' + $kept)
    if ($ctx.Adapter.Value.Connected) { Add-Warning 'The adapter is still CONNECTED and now has no isolation. Run -Action Disconnect or -Action Apply before starting the VM.' }
    Set-Outcome -Result 'removed-read-back-confirmed' -Detail ('removed ' + $rm.RemovedCount)
}

function Invoke-ConnectAction {
    $ctx = Get-VmContext -NeedHostNetwork
    if ($ctx.Adapter.Status -ne 'MEASURED' -or $ctx.Gateway.Status -ne 'MEASURED' -or $ctx.Lan.Status -ne 'MEASURED') {
        Stop-Refused 'isolation not applied (or not measurable), refusing to connect: the adapter, gateway or host LAN state could not be measured.'
        return
    }
    if ($ctx.Adapter.Value.Connected -and $ctx.Adapter.Value.SwitchName -eq $script:SwitchName) {
        Write-Host ("The adapter is already connected to '" + $script:SwitchName + "'. Showing the isolation state instead:")
        Invoke-ShowAction
        return
    }
    $inv = $null
    try { $inv = Read-Inventory } catch { Stop-Refused ('isolation not applied, refusing to connect: the inventory is unreadable (' + (Limit-Text -Text $_.Exception.Message) + ').'); return }
    if ($null -eq $inv) { Stop-Refused 'isolation not applied, refusing to connect: there is no inventory. Run -Action Apply first.'; return }
    $rb = Invoke-Measure -What 'Extended ACL rules (read-back before connecting)' -Method 'Get-VMNetworkAdapterExtendedAcl -VMName' -Probe {
        $r = @(Get-AclReadBack)
        New-MeasureResult -Value $r -Display ($r.Count.ToString() + ' rule(s)')
    }
    if ($rb.Status -ne 'MEASURED') { Stop-Refused 'isolation not applied (read-back failed), refusing to connect.'; return }
    $present = @($rb.Value)
    Show-AclTable -Rules $present
    $cmp = Compare-WithInventory -Inventory $inv -Present $present
    Show-InventoryComparison -Cmp $cmp
    if (@($cmp.BaselineMissing).Count -gt 0) { Stop-Refused ('isolation not applied, refusing to connect: ' + @($cmp.BaselineMissing).Count + ' baseline deny rule(s) are missing from the adapter. Run -Action Apply.'); return }
    if (@($cmp.Missing).Count -gt 0) { Stop-Refused ('isolation not applied completely, refusing to connect: ' + @($cmp.Missing).Count + ' inventoried rule(s) are missing. Run -Action Apply.'); return }
    if ([string](Get-PropValue -Object $inv -Name 'State') -ne 'applied-and-read-back') { Stop-Refused ("the inventory state is '" + (Get-PropValue -Object $inv -Name 'State') + "', not 'applied-and-read-back'. Run -Action Apply."); return }

    # The Default Switch gets a new subnet after host reboots: rules built for the old gateway would be stale.
    $gwNow = $ctx.Gateway.Value.Ip
    $gwInv = [string](Get-PropValue -Object $inv -Name 'GatewayIp')
    if ($gwNow -ne $gwInv) {
        Stop-Refused ("the Default Switch gateway is now " + $gwNow + " but the rules were built for " + $gwInv + " (the Default Switch changes address after some host reboots). The guest would have no DNS and the host could not reach port 7001. Run -Action Apply again (the adapter is disconnected, so it may).")
        return
    }
    # Every current host LAN address must be covered by an inventoried outbound deny range.
    $denyRanges = New-Object System.Collections.Generic.List[object]
    foreach ($r in @(Get-InventoryRuleList -Inventory $inv)) {
        if ($r.Direction -eq 'Outbound' -and $r.Action -eq 'Deny' -and $r.RemoteIPAddress -match '^\d{1,3}(\.\d{1,3}){3}(/\d{1,2})?$') { $denyRanges.Add((Get-Ipv4Range -Text $r.RemoteIPAddress)) }
    }
    foreach ($h in @($ctx.Lan.Value)) {
        $hr = Get-Ipv4Range -Text ($h.Ip + '/32')
        $covered = $false
        foreach ($d in $denyRanges) { if (Test-RangeContains -Outer $d -Inner $hr) { $covered = $true } }
        if (-not $covered) { Stop-Refused ('isolation not applied for the host LAN address ' + $h.Ip + ' (it changed since Apply and no deny rule covers it). Run -Action Apply again.'); return }
    }

    if (-not $script:Cmdlet.ShouldProcess("VM network adapter of '" + $Name + "'", "Connect to switch '" + $script:SwitchName + "'")) {
        $script:SkipReport = $true
        Write-Host 'WhatIf/Confirm: no changes were made. The isolation checks above passed.' -ForegroundColor Yellow
        Set-Outcome -Result 'whatif' -Detail 'no changes'
        return
    }
    Connect-VMNetworkAdapter -VMName $Name -SwitchName $script:SwitchName
    $after = Invoke-Measure -What 'Adapter SwitchName and Connected after Connect' -Method 'Get-VMNetworkAdapter (SwitchName, Connected)' -Probe {
        $info = Get-AdapterInfo
        New-MeasureResult -Value $info -Display ("SwitchName = {0}, Connected = {1}" -f $info.SwitchName, $info.Connected)
    }
    if ($after.Status -eq 'MEASURED' -and $after.Value.Connected -and $after.Value.SwitchName -eq $script:SwitchName) {
        Set-Outcome -Result 'connected-read-back-confirmed' -Detail ('connected to ' + $script:SwitchName)
    }
    else {
        Set-Outcome -Result 'connect-not-confirmed' -Detail 'read-back does not show the adapter connected' -Code 1
    }
}

function Invoke-DisconnectAction {
    $ctx = Get-VmContext
    if ($ctx.Adapter.Status -ne 'MEASURED') { Stop-Refused 'the adapter state could not be measured.'; return }
    if (-not $ctx.Adapter.Value.Connected) {
        Write-Host 'The adapter is already disconnected (Get-VMNetworkAdapter Connected = False). Nothing to do.'
        Set-Outcome -Result 'noop-already-disconnected' -Detail 'already disconnected'
        return
    }
    if (-not $script:Cmdlet.ShouldProcess("VM network adapter of '" + $Name + "'", 'Disconnect from the virtual switch')) {
        $script:SkipReport = $true
        Write-Host 'WhatIf/Confirm: no changes were made.' -ForegroundColor Yellow
        Set-Outcome -Result 'whatif' -Detail 'no changes'
        return
    }
    Disconnect-VMNetworkAdapter -VMName $Name
    $after = Invoke-Measure -What 'Adapter SwitchName and Connected after Disconnect' -Method 'Get-VMNetworkAdapter (SwitchName, Connected)' -Probe {
        $info = Get-AdapterInfo
        New-MeasureResult -Value $info -Display ("SwitchName = '{0}', Connected = {1}" -f $info.SwitchName, $info.Connected)
    }
    if ($after.Status -eq 'MEASURED' -and -not $after.Value.Connected) { Set-Outcome -Result 'disconnected-read-back-confirmed' -Detail 'adapter disconnected' }
    else { Set-Outcome -Result 'disconnect-not-confirmed' -Detail 'read-back does not show the adapter disconnected' -Code 1 }
}

function Initialize-HyperV {
    if (-not (Test-CanManageHyperV)) {
        Stop-Refused 'this needs an elevated PowerShell (or membership of the Hyper-V Administrators group). Open one: Start menu, type PowerShell, right-click Windows PowerShell, Run as administrator.'
        return $false
    }
    try {
        Import-Module -Name 'Hyper-V' -ErrorAction Stop | Out-Null
    }
    catch {
        Stop-Refused ('the Hyper-V PowerShell module could not be loaded: ' + (Limit-Text -Text $_.Exception.Message) + ' Run Preflight-Workbench.ps1.')
        return $false
    }
    return $true
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
try {
    Write-Host ('AVNI Agent Workbench - network isolation - ' + $Action + " - '" + $Name + "' - " + (Get-Stamp)) -ForegroundColor White
    if (Initialize-HyperV) {
        switch ($Action) {
            'Apply'      { Invoke-ApplyAction }
            'Show'       { Invoke-ShowAction }
            'Remove'     { Invoke-RemoveAction }
            'Connect'    { Invoke-ConnectAction }
            'Disconnect' { Invoke-DisconnectAction }
        }
    }
}
catch {
    $message = Limit-Text -Text $_.Exception.Message -Max 400
    Write-Host ('FAILED: ' + $message) -ForegroundColor Red
    Set-Outcome -Result 'error' -Detail $message -Code 1
}
finally {
    Save-Report
}
exit $script:ExitCode
