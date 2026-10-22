<#
.SYNOPSIS
    AVNI Agent Workbench - bring the Workbench up safely after any PC restart. Run elevated. One command, every step measured.

.DESCRIPTION
    Why: this PC is powered off every evening. On each boot Windows gives the Hyper-V Default Switch a NEW address
    (measured 21-10-2026: 172.17.224.1 -> 172.26.32.1; 22-10-2026: -> 172.20.80.1). The guest's network rules are
    built for one address, so after a boot the guest gets no network until the rules are rebuilt.

    Steps (each one printed with a time stamp):
      1. Read the live switch address and the address the rules were built for.
      2. If they differ (or -Force): Disconnect the adapter, Apply the rules for the live address, Show (inventory must
         MATCH), Connect. Set-WorkbenchNetworkIsolation.ps1 refuses Connect when any deny rule is missing.
      3. Start the VM if it is Off, wait for PowerShell Direct (no network needed), renew the guest's address.
      4. Measure from inside the guest: its address, internet on TCP 443.
      5. Measure from the host: hands service /health with the token = 200, without = 401.
      6. Write the guest address to %LOCALAPPDATA%\Avni\workbench\guest-ip.txt (the Claude Code bridge reads it).
      7. Unless -SkipVerify: Test-WorkbenchIsolation.ps1 (office LAN unreachable, internet and DNS work).
    Exit 0 = ready. Any other exit = not ready, with the reason printed.
#>
[CmdletBinding()]
param(
    [string]$Name = 'AVNI Agent Workbench 01',
    [switch]$Force,
    [switch]$SkipVerify
)
$ErrorActionPreference = 'Continue'
function Stamp { (Get-Date).ToString('dd-MM-yyyy HH:mm:ss') }
function Say($t) { Write-Output "$(Stamp) $t" }
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$base = Join-Path $env:LOCALAPPDATA 'Avni\workbench'
Set-Location $here

# 1. addresses
$gwLive = (Get-NetIPAddress -InterfaceAlias 'vEthernet (Default Switch)' -AddressFamily IPv4 -ErrorAction SilentlyContinue).IPAddress
$invFile = Join-Path $base ('isolation-' + ($Name -replace '[^A-Za-z0-9]', '_') + '.json')
$gwRules = if (Test-Path $invFile) { (Get-Content $invFile -Raw | ConvertFrom-Json).GatewayIp } else { $null }
Say "switch address now: $gwLive | rules built for: $gwRules | PC booted: $((Get-CimInstance Win32_OperatingSystem).LastBootUpTime)"
if (-not $gwLive) { Say 'NOT READY: the Hyper-V Default Switch has no IPv4 address (Hyper-V not running?)'; exit 10 }

# 2. heal the rules when the address moved
if ($Force -or $gwLive -ne $gwRules) {
    Say 'address changed - rebuilding the rules: Disconnect, Apply, Show, Connect'
    & (Join-Path $here 'Set-WorkbenchNetworkIsolation.ps1') -Name $Name -Action Disconnect *> $null
    $apply = & (Join-Path $here 'Set-WorkbenchNetworkIsolation.ps1') -Name $Name -Action Apply *>&1 | Out-String
    $applyExit = $LASTEXITCODE
    ($apply -split "`n") | Where-Object { $_ -match 'present in the read-back|REFUSED|FAILED' } | ForEach-Object { Say ('apply: ' + $_.Trim()) }
    if ($applyExit -ne 0) { Say "NOT READY: Apply exit $applyExit"; exit 11 }
    $show = & (Join-Path $here 'Set-WorkbenchNetworkIsolation.ps1') -Name $Name -Action Show *>&1 | Out-String
    if ($show -notmatch 'inventory MATCHES') { Say 'NOT READY: Show did not report inventory MATCHES'; exit 12 }
    Say 'show: inventory MATCHES the adapter'
}
$ad = Get-VMNetworkAdapter -VMName $Name
if (-not $ad.Connected) {
    $con = & (Join-Path $here 'Set-WorkbenchNetworkIsolation.ps1') -Name $Name -Action Connect *>&1 | Out-String
    if ($LASTEXITCODE -ne 0) { Say "NOT READY: Connect refused or failed (exit $LASTEXITCODE)"; exit 13 }
}
Say ("adapter: switch '" + (Get-VMNetworkAdapter -VMName $Name).SwitchName + "', connected " + (Get-VMNetworkAdapter -VMName $Name).Connected)

# 3. VM up, PowerShell Direct, renew
$vm = Get-VM -Name $Name
if ($vm.State -ne 'Running') { Say "VM was $($vm.State) - starting"; Start-VM -Name $Name }
$acct = Get-Content (Join-Path $base 'guest-account.json') -Raw | ConvertFrom-Json
$cred = New-Object System.Management.Automation.PSCredential (($acct.computer + '\' + $acct.user), (ConvertTo-SecureString $acct.password -AsPlainText -Force))
$deadline = (Get-Date).AddMinutes(15); $net = $null
while ((Get-Date) -lt $deadline) {
    try {
        $net = Invoke-Command -VMName $Name -Credential $cred -ErrorAction Stop -ScriptBlock {
            ipconfig /release *> $null; ipconfig /renew *> $null; Start-Sleep -Seconds 3
            $ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -notlike '127.*' -and $_.IPAddress -notlike '169.254.*' } | Select-Object -First 1).IPAddress
            # The host address moves with every PC boot. The hands firewall rule allows exactly one host address, and the
            # hands service reads its one allowed host (the gateway) when it starts. Measured 22-10-2026: without this
            # step the guest refuses the host after every restart. Point the rule at today's host, restart the service.
            $gw = (Get-NetRoute -DestinationPrefix '0.0.0.0/0' | Sort-Object RouteMetric, InterfaceMetric | Select-Object -First 1).NextHop
            $rule = Get-NetFirewallRule -DisplayName 'AVNI Hands 7001' -ErrorAction SilentlyContinue
            $was = if ($rule) { (($rule | Get-NetFirewallAddressFilter).RemoteAddress) -join ',' } else { '(no rule)' }
            if ($rule -and $was -ne $gw) { $rule | Set-NetFirewallRule -RemoteAddress $gw }
            Stop-ScheduledTask -TaskPath '\AVNI\' -TaskName 'Hands' -ErrorAction SilentlyContinue
            Get-CimInstance Win32_Process -Filter "Name='pythonw.exe'" | Where-Object { $_.CommandLine -like '*avni_hands.py*' } | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
            Start-ScheduledTask -TaskPath '\AVNI\' -TaskName 'Hands' -ErrorAction SilentlyContinue
            Start-Sleep -Seconds 5
            [pscustomobject]@{ Ip = $ip; Gateway = $gw; RuleWas = $was; Internet = (Test-NetConnection 1.1.1.1 -Port 443 -WarningAction SilentlyContinue).TcpTestSucceeded
                               Hands = (Get-ScheduledTask -TaskPath '\AVNI\' -TaskName 'Hands' -ErrorAction SilentlyContinue).State }
        }
        if ($net.Ip -and $net.Internet) { break }
    } catch { }
    Start-Sleep -Seconds 15
}
if (-not $net -or -not $net.Ip) { Say 'NOT READY: the guest did not answer PowerShell Direct within 15 min'; exit 14 }
Say "guest: address $($net.Ip), host $($net.Gateway) (hands rule was $($net.RuleWas)), internet on 443 $($net.Internet), hands task $($net.Hands)"
if (-not $net.Internet) { Say 'NOT READY: the guest has no internet'; exit 15 }

# 5. hands from the host
$tok = (Get-Content (Join-Path $base 'hands.token') -Raw).Trim()
function Get-Code([string]$url, [hashtable]$hdr) {
    try { return [int](Invoke-WebRequest $url -Headers $hdr -UseBasicParsing -TimeoutSec 15).StatusCode }
    catch { if ($_.Exception.Response) { return [int]$_.Exception.Response.StatusCode } else { return 'no answer' } }
}
$h200 = Get-Code "http://$($net.Ip):7001/health" @{ 'X-Avni-Token' = $tok }
$h401 = Get-Code "http://$($net.Ip):7001/health" @{}
Say "hands from the host: with token $h200 (want 200), without token $h401 (want 401)"
if ($h200 -ne 200 -or $h401 -ne 401) {
    Say 'hands not answering as expected - starting the guest task \AVNI\Hands once'
    Invoke-Command -VMName $Name -Credential $cred -ScriptBlock { Start-ScheduledTask -TaskPath '\AVNI\' -TaskName 'Hands' } -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 8
    $h200 = Get-Code "http://$($net.Ip):7001/health" @{ 'X-Avni-Token' = $tok }
    Say "hands after restart: $h200"
    if ($h200 -ne 200) { Say 'NOT READY: hands service does not answer'; exit 16 }
}

# 6. address for the bridge
Set-Content -Path (Join-Path $base 'guest-ip.txt') -Value $net.Ip -Encoding ascii
Say "guest address written to $(Join-Path $base 'guest-ip.txt')"

# 7. isolation proof
if (-not $SkipVerify) {
    $t = & (Join-Path $here 'Test-WorkbenchIsolation.ps1') -Name $Name -Credential $cred *>&1 | Out-String
    $code = $LASTEXITCODE
    ($t -split "`n") | Where-Object { $_ -match '^(ISOLATION|NOT MEASURED)|Report written' } | ForEach-Object { Say ('isolation: ' + $_.Trim()) }
    if ($code -ne 0) { Say "NOT READY: isolation test exit $code"; exit 17 }
}
Say 'READY - the Workbench is up, isolated and its hands answer'
exit 0
