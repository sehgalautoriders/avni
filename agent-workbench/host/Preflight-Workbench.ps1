#Requires -Version 5.1
<#
.SYNOPSIS
Read-only check: can this Windows PC host the AVNI Agent Workbench (a Hyper-V Windows 11 VM)?

.DESCRIPTION
This script LOOKS at the computer and writes a report. It changes nothing.

What it does:
  - Measures the Windows edition, CPU virtualisation flags, Hyper-V feature and
    services, existing VMs and switches, free RAM, free disk space on every
    fixed drive, other hypervisors (VirtualBox, VMware, WSL, Docker) and the GPU.
  - Prints one block per measurement: what was measured, the value, the method
    (cmdlet or WMI class) and the time. If something could not be measured it
    says "NOT MEASURED" and why (for example "needs admin"). It never prints a
    "good" status just because a folder or an .exe exists.
  - Ends with a VERDICT: READY FOR STAGE 1, READY WITH CONDITIONS, or NOT READY.
    Every line that drives the verdict quotes the measurement behind it.
  - Saves a JSON file (all raw values) and a readable .txt copy to
    %LOCALAPPDATA%\Avni\reports\ named like workbench-preflight-dd-MM-yyyy_HHmm.json

What it never does:
  - It never creates, starts, stops or deletes a VM, a switch, a file or a setting.
  - It never installs or enables anything (no Enable-WindowsOptionalFeature).
  - It never writes to Desktop, Documents or any project folder. The only folder
    it creates is %LOCALAPPDATA%\Avni\reports\.
  - It never stops, starts or touches any process other than its own short-lived
    helper programs (systeminfo.exe, wsl.exe). It never touches AutoHotkey.
  - It contains no secrets and sends nothing over the network.

Administrator rights are NOT required. Without them, a few items (the Hyper-V
feature state, the VM and switch lists) print "NOT MEASURED (needs admin)" and
the verdict becomes "READY WITH CONDITIONS" at best. Run it again from an
elevated PowerShell for the full picture.

Exit code: 0 always (it is a diagnostic), except 2 if the report files could
not be written.

.PARAMETER SkipSystemInfo
Skip the systeminfo.exe call (it can take 10-30 seconds). The item is then
reported as NOT MEASURED.

.EXAMPLE
.\Preflight-Workbench.ps1
Run the diagnostics from a normal PowerShell window.

.EXAMPLE
.\Preflight-Workbench.ps1 -SkipSystemInfo
Faster run that does not call systeminfo.exe.

.NOTES
Works in Windows PowerShell 5.1 and PowerShell 7 on Windows.
Dates in file names are dd-MM-yyyy.
#>
[CmdletBinding()]
param(
    [switch]$SkipSystemInfo
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Script-wide state
# ---------------------------------------------------------------------------
$script:ToolName          = 'Preflight-Workbench'
$script:ToolVersion       = '1.0.0'
$script:Culture           = [System.Globalization.CultureInfo]::InvariantCulture
$script:Lines             = New-Object System.Collections.Generic.List[string]
$script:Records           = New-Object System.Collections.Generic.List[object]
$script:Index             = @{}
$script:Findings          = New-Object System.Collections.Generic.List[object]
$script:Notes             = New-Object System.Collections.Generic.List[string]
$script:IsAdmin           = $false
$script:InstalledPrograms = $null
$script:RunStarted        = Get-Date
$script:Verdict           = $null
$script:HostHeadroomGB    = 4
$script:DefaultVmName     = 'AVNI Agent Workbench 01'

# ---------------------------------------------------------------------------
# Small helpers
# ---------------------------------------------------------------------------
function Get-Stamp {
    return (Get-Date).ToString('yyyy-MM-dd HH:mm:ss', $script:Culture)
}

function Get-PropValue {
    # Strict-mode safe property read: returns $null when the property is absent.
    param($Object, [string]$Name)
    if ($null -eq $Object) { return $null }
    if ($Object.PSObject.Properties.Match($Name).Count -gt 0) { return $Object.$Name }
    return $null
}

function Format-GB {
    param($Gb)
    if ($null -eq $Gb) { return 'n/a' }
    return ([double]$Gb).ToString('0.0', $script:Culture)
}

function Limit-Text {
    param([string]$Text, [int]$Max = 100)
    if ($null -eq $Text) { return '' }
    $flat = ($Text -replace '\s+', ' ').Trim()
    if ($flat.Length -le $Max) { return $flat }
    return $flat.Substring(0, $Max - 3) + '...'
}

function Test-IsAdmin {
    try {
        $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
        return $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
    }
    catch {
        return $false
    }
}

function Test-CanQueryHyperV {
    # True for an elevated administrator or a member of the "Hyper-V Administrators" group.
    if ($script:IsAdmin) { return $true }
    try {
        $identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
        $sid = New-Object System.Security.Principal.SecurityIdentifier('S-1-5-32-578')
        return $principal.IsInRole($sid)
    }
    catch {
        return $false
    }
}

function Resolve-BoolList {
    # All true -> $true; any false -> $false; nothing reported -> $null.
    param($List)
    $values = @($List | Where-Object { $null -ne $_ })
    if ($values.Count -eq 0) { return $null }
    foreach ($v in $values) {
        if (-not [bool]$v) { return $false }
    }
    return $true
}

function Repair-NativeText {
    # Native tools are read as raw bytes (Latin-1 is a 1:1 byte mapping), then decoded
    # properly: UTF-16 (wsl.exe) when NUL bytes are present, otherwise UTF-8.
    param([string]$Latin1Text)
    if ([string]::IsNullOrEmpty($Latin1Text)) { return '' }
    $latin1 = [System.Text.Encoding]::GetEncoding('iso-8859-1')
    $bytes = $latin1.GetBytes($Latin1Text)
    if ($Latin1Text.IndexOf([char]0) -ge 0) {
        $text = [System.Text.Encoding]::Unicode.GetString($bytes)
    }
    else {
        $text = [System.Text.Encoding]::UTF8.GetString($bytes)
    }
    return $text.Replace([string][char]0xFEFF, '')
}

function Invoke-NativeCapture {
    # Runs a console program, captures stdout/stderr, enforces a timeout.
    # Only ever terminates the helper process that this function itself started.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string]$Arguments = '',
        [int]$TimeoutSec = 60
    )
    $latin1 = [System.Text.Encoding]::GetEncoding('iso-8859-1')
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $FilePath
    $psi.Arguments = $Arguments
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $psi.StandardOutputEncoding = $latin1
    $psi.StandardErrorEncoding = $latin1
    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    try {
        [void]$proc.Start()
        $outTask = $proc.StandardOutput.ReadToEndAsync()
        $errTask = $proc.StandardError.ReadToEndAsync()
        if (-not $proc.WaitForExit($TimeoutSec * 1000)) {
            try { $proc.Kill() } catch { Write-Verbose 'helper process already gone' }
            throw "REASON: $FilePath did not finish within $TimeoutSec seconds"
        }
        $proc.WaitForExit()
        return [pscustomobject]@{
            ExitCode = $proc.ExitCode
            StdOut   = (Repair-NativeText -Latin1Text $outTask.Result)
            StdErr   = (Repair-NativeText -Latin1Text $errTask.Result)
        }
    }
    finally {
        $proc.Dispose()
    }
}

# ---------------------------------------------------------------------------
# Output (every printed line is also kept for the .txt report)
# ---------------------------------------------------------------------------
function Write-ReportLine {
    param([string]$Text = '', [string]$Color = '')
    $script:Lines.Add($Text)
    if ([string]::IsNullOrEmpty($Color)) { Write-Host $Text } else { Write-Host $Text -ForegroundColor $Color }
}

function Write-Section {
    param([string]$Title)
    Write-ReportLine ''
    Write-ReportLine ('-' * 72)
    Write-ReportLine $Title
    Write-ReportLine ('-' * 72)
}

function Write-Note {
    param([string]$Text)
    $script:Notes.Add($Text)
    Write-ReportLine ("NOTE: " + $Text) 'DarkYellow'
}

function Write-Measurement {
    param([Parameter(Mandatory)]$Record)
    if ($Record.Status -eq 'MEASURED') {
        $tag = '[' + $Record.Kind + ']'
        $color = 'Cyan'
    }
    else {
        $tag = '[NOT MEASURED]'
        $color = 'Yellow'
    }
    $displayLines = @($Record.Display -split "`n")
    Write-ReportLine ('{0,-14} {1}: {2}' -f $tag, $Record.What, $displayLines[0]) $color
    for ($i = 1; $i -lt $displayLines.Count; $i++) {
        Write-ReportLine ('{0,-14}   {1}' -f '', $displayLines[$i]) $color
    }
    Write-ReportLine ('{0,-14}   method: {1} | at {2}' -f '', $Record.Method, $Record.MeasuredAt) 'DarkGray'
}

# ---------------------------------------------------------------------------
# Measurement engine
# ---------------------------------------------------------------------------
function New-MeasureResult {
    # Every measurement probe returns exactly one of these.
    param($Value, [string]$Display = '')
    $result = [pscustomobject]@{ Value = $Value; Display = $Display }
    $result.PSObject.TypeNames.Insert(0, 'Avni.MeasureResult')
    return $result
}

function ConvertTo-DisplayText {
    param($Value)
    if ($null -eq $Value) { return 'NOT MEASURED' }
    if ($Value -is [string]) { return $Value }
    if ($Value -is [System.ValueType]) { return [string]$Value }
    if ($Value -is [System.Collections.IEnumerable] -and -not ($Value -is [System.Collections.IDictionary])) {
        $parts = @(foreach ($item in $Value) { ConvertTo-DisplayText -Value $item })
        return ($parts -join '; ')
    }
    return (ConvertTo-Json -InputObject $Value -Compress -Depth 4)
}

function Get-NotMeasuredReason {
    param([string]$Message)
    if ($Message -eq 'NEEDS_ADMIN') { return 'needs admin' }
    if ($Message -match '^REASON:\s*(.+)$') { return $Matches[1].Trim() }
    if ($Message -match 'elevat|administrator|Access is denied|UnauthorizedAccess|0x800702E4|required permission') {
        return 'needs admin'
    }
    return ('error: ' + (Limit-Text -Text $Message -Max 160))
}

function Register-Record {
    param([Parameter(Mandatory)]$Record)
    $script:Records.Add($Record)
    $script:Index[$Record.Id] = $Record
    Write-Measurement -Record $Record
}

function Invoke-Measure {
    # Runs one probe inside try/catch so one failure never kills the report.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][string]$What,
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][scriptblock]$Probe,
        [switch]$NeedsAdmin
    )
    $mxAt = Get-Stamp
    $mxValue = $null
    $mxDisplay = ''
    $mxStatus = 'MEASURED'
    $mxReason = $null
    try {
        if ($NeedsAdmin -and -not $script:IsAdmin) { throw 'NEEDS_ADMIN' }
        $mxOut = @(& $Probe)
        $mxResult = $null
        foreach ($mxItem in $mxOut) {
            if ($null -ne $mxItem -and ($mxItem.PSObject.TypeNames -contains 'Avni.MeasureResult')) { $mxResult = $mxItem }
        }
        if ($null -eq $mxResult) { throw 'REASON: the probe returned no value' }
        $mxValue = $mxResult.Value
        $mxDisplay = [string]$mxResult.Display
        if ([string]::IsNullOrEmpty($mxDisplay)) { $mxDisplay = ConvertTo-DisplayText -Value $mxValue }
    }
    catch {
        $mxStatus = 'NOT MEASURED'
        $mxReason = Get-NotMeasuredReason -Message $_.Exception.Message
        $mxValue = $null
        $mxDisplay = "NOT MEASURED ($mxReason)"
    }
    $record = [pscustomobject]([ordered]@{
        Id         = $Id
        Kind       = 'MEASURED'
        What       = $What
        Status     = $mxStatus
        Value      = $mxValue
        Display    = $mxDisplay
        Reason     = $mxReason
        Method     = $Method
        MeasuredAt = $mxAt
    })
    Register-Record -Record $record
}

function Add-Derived {
    # A value computed by a stated rule from measurements. Marked DERIVED, never MEASURED.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$Id,
        [Parameter(Mandatory)][string]$What,
        [Parameter(Mandatory)][string]$Rule,
        [string[]]$From = @(),
        $Value = $null,
        [string]$Display = '',
        [string]$NotMeasuredReason = 'an input was not measured'
    )
    $status = 'MEASURED'
    $reason = $null
    if ($null -eq $Value) {
        $status = 'NOT MEASURED'
        $reason = $NotMeasuredReason
        $Display = "NOT MEASURED ($reason)"
    }
    elseif ([string]::IsNullOrEmpty($Display)) {
        $Display = ConvertTo-DisplayText -Value $Value
    }
    $record = [pscustomobject]([ordered]@{
        Id         = $Id
        Kind       = 'DERIVED'
        What       = $What
        Status     = $status
        Value      = $Value
        Display    = $Display
        Reason     = $reason
        Method     = ('derived by rule: ' + $Rule + ' | from: ' + ($From -join ', '))
        MeasuredAt = (Get-Stamp)
    })
    Register-Record -Record $record
}

function Test-Measured {
    param([string]$Id)
    return ($script:Index.ContainsKey($Id) -and $script:Index[$Id].Status -eq 'MEASURED')
}

function Get-MeasuredScalar {
    # Scalar value of a measurement, or $null when it was not measured. Do not use for arrays.
    param([string]$Id)
    if (Test-Measured -Id $Id) { return $script:Index[$Id].Value }
    return $null
}

function Get-Cite {
    param([string]$Id)
    if (-not $script:Index.ContainsKey($Id)) { return ('[' + $Id + ': not run]') }
    $r = $script:Index[$Id]
    return ('[{0}: {1} | {2} | {3}]' -f $Id, (Limit-Text -Text $r.Display -Max 90), (Limit-Text -Text $r.Method -Max 70), $r.MeasuredAt)
}

# ---------------------------------------------------------------------------
# Installed-program lookup (registry uninstall keys; presence only)
# ---------------------------------------------------------------------------
function Get-InstalledProgramList {
    if ($null -eq $script:InstalledPrograms) {
        $names = New-Object System.Collections.Generic.List[string]
        $keys = @(
            'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
            'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
        )
        foreach ($key in $keys) {
            $items = @(Get-ItemProperty -Path $key -ErrorAction SilentlyContinue)
            foreach ($item in $items) {
                $display = Get-PropValue -Object $item -Name 'DisplayName'
                if ($display) { $names.Add([string]$display) }
            }
        }
        $script:InstalledPrograms = $names.ToArray()
    }
    return $script:InstalledPrograms
}

function Get-PresenceEvidence {
    # Returns a list of human-readable evidence strings (empty list = not found by these methods).
    param([string]$NamePattern, [string[]]$Paths)
    $evidence = New-Object System.Collections.Generic.List[string]
    foreach ($name in @(Get-InstalledProgramList)) {
        if ($name -match $NamePattern) { $evidence.Add("uninstall registry entry '$name'") }
    }
    foreach ($path in $Paths) {
        if ($path -and (Test-Path -LiteralPath $path)) { $evidence.Add("path exists: $path") }
    }
    return $evidence.ToArray()
}

# ---------------------------------------------------------------------------
# Measurements
# ---------------------------------------------------------------------------
function Get-HyperVEditionSupport {
    param([string]$EditionId)
    if ([string]::IsNullOrWhiteSpace($EditionId)) { return 'UNKNOWN' }
    if ($EditionId -match '^Core') { return 'NO' }
    if ($EditionId -match '^(Professional|Enterprise|Education|IoTEnterprise|ServerStandard|ServerDatacenter)') { return 'YES' }
    return 'UNKNOWN'
}

function Measure-HostIdentity {
    Invoke-Measure -Id 'host.admin' -What 'Running elevated (administrator)' `
        -Method 'WindowsPrincipal.IsInRole(Administrator)' -Probe {
        New-MeasureResult -Value $script:IsAdmin -Display ([string]$script:IsAdmin)
    }
    Invoke-Measure -Id 'host.powershell' -What 'PowerShell host version' `
        -Method '$PSVersionTable' -Probe {
        $edition = 'Desktop'
        if ($PSVersionTable.ContainsKey('PSEdition')) { $edition = [string]$PSVersionTable['PSEdition'] }
        $text = "{0} ({1})" -f $PSVersionTable.PSVersion.ToString(), $edition
        New-MeasureResult -Value $text -Display $text
    }
}

function Measure-WindowsEdition {
    Invoke-Measure -Id 'os.info' -What 'Windows edition, version, build' `
        -Method 'Get-CimInstance Win32_OperatingSystem; Get-ItemProperty HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Probe {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $cv = Get-ItemProperty -Path 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
        $displayVersion = Get-PropValue -Object $cv -Name 'DisplayVersion'
        if (-not $displayVersion) { $displayVersion = Get-PropValue -Object $cv -Name 'ReleaseId' }
        $info = [pscustomobject]([ordered]@{
            Caption        = [string]$os.Caption
            Version        = [string]$os.Version
            BuildNumber    = [string]$os.BuildNumber
            UBR            = Get-PropValue -Object $cv -Name 'UBR'
            DisplayVersion = $displayVersion
            EditionID      = Get-PropValue -Object $cv -Name 'EditionID'
            OSArchitecture = [string]$os.OSArchitecture
        })
        $text = '{0} | version {1} | build {2}.{3} | DisplayVersion {4} | EditionID {5} | {6}' -f `
            $info.Caption, $info.Version, $info.BuildNumber, $info.UBR, $info.DisplayVersion, $info.EditionID, $info.OSArchitecture
        New-MeasureResult -Value $info -Display $text
    }
    $from = @('os.info')
    if (Test-Measured -Id 'os.info') {
        $editionId = [string]$script:Index['os.info'].Value.EditionID
        $support = Get-HyperVEditionSupport -EditionId $editionId
        $text = "Hyper-V supported by this edition: $support (EditionID '$editionId'; Pro/Enterprise/Education = YES, Home = NO)"
        Add-Derived -Id 'os.hyperv_edition' -What 'Edition supports Hyper-V' -Rule 'EditionID prefix: Professional/Enterprise/Education = YES, Core(Home) = NO' `
            -From $from -Value $support -Display $text
    }
    else {
        Add-Derived -Id 'os.hyperv_edition' -What 'Edition supports Hyper-V' -Rule 'EditionID prefix' -From $from `
            -Value $null -NotMeasuredReason 'Windows edition was not measured'
    }
}

function Measure-Cpu {
    Invoke-Measure -Id 'cpu.name' -What 'CPU name' -Method 'Get-CimInstance Win32_Processor (Name)' -Probe {
        $cpus = @(Get-CimInstance -ClassName Win32_Processor)
        $names = @($cpus | ForEach-Object { ([string]$_.Name).Trim() })
        $text = $names -join '; '
        New-MeasureResult -Value $text -Display $text
    }
    Invoke-Measure -Id 'cpu.cores' -What 'CPU sockets, physical cores, logical processors' `
        -Method 'Get-CimInstance Win32_Processor (NumberOfCores, NumberOfLogicalProcessors)' -Probe {
        $cpus = @(Get-CimInstance -ClassName Win32_Processor)
        $cores = ($cpus | Measure-Object -Property NumberOfCores -Sum).Sum
        $logical = ($cpus | Measure-Object -Property NumberOfLogicalProcessors -Sum).Sum
        $info = [pscustomobject]([ordered]@{ Sockets = $cpus.Count; PhysicalCores = $cores; LogicalProcessors = $logical })
        New-MeasureResult -Value $info -Display ("{0} socket(s), {1} physical core(s), {2} logical processor(s)" -f $cpus.Count, $cores, $logical)
    }
    $specs = @(
        @{ Id = 'cpu.virt_fw'; Prop = 'VirtualizationFirmwareEnabled' },
        @{ Id = 'cpu.slat';    Prop = 'SecondLevelAddressTranslationExtensions' },
        @{ Id = 'cpu.vmmode';  Prop = 'VMMonitorModeExtensions' }
    )
    foreach ($spec in $specs) {
        $propName = $spec.Prop
        Invoke-Measure -Id $spec.Id -What "CPU $propName" `
            -Method "Get-CimInstance Win32_Processor ($propName)" -Probe {
            $cpus = @(Get-CimInstance -ClassName Win32_Processor)
            $values = @($cpus | ForEach-Object { Get-PropValue -Object $_ -Name $propName })
            $all = Resolve-BoolList -List $values
            if ($null -eq $all) { throw 'REASON: WMI did not report this property on this machine' }
            New-MeasureResult -Value $all -Display ([string]$all)
        }
    }
    Invoke-Measure -Id 'cpu.hypervisor_present' -What 'A hypervisor is already running on this host' `
        -Method 'Get-CimInstance Win32_ComputerSystem (HypervisorPresent)' -Probe {
        $cs = Get-CimInstance -ClassName Win32_ComputerSystem
        $present = Get-PropValue -Object $cs -Name 'HypervisorPresent'
        if ($null -eq $present) { throw 'REASON: HypervisorPresent not reported by WMI' }
        New-MeasureResult -Value ([bool]$present) -Display ([string][bool]$present)
    }
    Write-Note 'When a hypervisor is already running (Hyper-V, WSL2, Virtual Machine Platform, VBS), Windows may report the firmware flags above as False even though virtualisation works. The verdict therefore trusts cpu.hypervisor_present first.'
}

function Measure-SystemInfo {
    Invoke-Measure -Id 'sys.hyperv_requirements' -What "systeminfo 'Hyper-V Requirements' section" `
        -Method 'systeminfo.exe (text section "Hyper-V Requirements"; English Windows only)' -Probe {
        if ($SkipSystemInfo) { throw 'REASON: skipped by -SkipSystemInfo' }
        $cap = Invoke-NativeCapture -FilePath 'systeminfo.exe' -TimeoutSec 120
        if ($cap.ExitCode -ne 0) { throw ("REASON: systeminfo.exe exit code " + $cap.ExitCode) }
        $all = @($cap.StdOut -split "\r?\n")
        $start = -1
        for ($i = 0; $i -lt $all.Count; $i++) {
            if ($all[$i] -match '^Hyper-V Requirements:') { $start = $i; break }
        }
        if ($start -lt 0) { throw 'REASON: section "Hyper-V Requirements" not found in systeminfo output (non-English Windows?)' }
        $found = New-Object System.Collections.Generic.List[string]
        $first = ($all[$start] -replace '^Hyper-V Requirements:\s*', '').Trim()
        if ($first) { $found.Add($first) }
        for ($j = $start + 1; $j -lt $all.Count; $j++) {
            if ($all[$j] -match '^\s+\S') { $found.Add($all[$j].Trim()) } else { break }
        }
        New-MeasureResult -Value $found.ToArray() -Display ($found.ToArray() -join "`n")
    }
    if (Test-Measured -Id 'sys.hyperv_requirements') {
        $joined = ($script:Index['sys.hyperv_requirements'].Value -join ' ')
        $detected = ($joined -match 'hypervisor has been detected')
        $text = "systeminfo says a hypervisor has been detected: $detected (True = Hyper-V/WSL2/VBS already on; its per-feature lines are then hidden by Windows)"
        Add-Derived -Id 'sys.hypervisor_detected' -What 'systeminfo reports a hypervisor' -Rule 'text match "A hypervisor has been detected"' `
            -From @('sys.hyperv_requirements') -Value $detected -Display $text
    }
    else {
        Add-Derived -Id 'sys.hypervisor_detected' -What 'systeminfo reports a hypervisor' -Rule 'text match "A hypervisor has been detected"' `
            -From @('sys.hyperv_requirements') -Value $null -NotMeasuredReason 'systeminfo section was not measured'
    }
}

function Measure-HyperVFeature {
    Invoke-Measure -Id 'hv.feature' -What 'Hyper-V optional feature (Microsoft-Hyper-V-All)' `
        -Method 'Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All' -NeedsAdmin -Probe {
        $feature = Get-WindowsOptionalFeature -Online -FeatureName 'Microsoft-Hyper-V-All'
        $state = [string]$feature.State
        New-MeasureResult -Value $state -Display $state
    }
    Invoke-Measure -Id 'hv.module' -What 'Hyper-V PowerShell module available' `
        -Method 'Get-Module -ListAvailable -Name Hyper-V' -Probe {
        $modules = @(Get-Module -ListAvailable -Name 'Hyper-V')
        if ($modules.Count -eq 0) {
            $info = [pscustomobject]([ordered]@{ Available = $false; Version = $null })
            New-MeasureResult -Value $info -Display 'NOT available (Get-Module -ListAvailable found no Hyper-V module)'
        }
        else {
            $top = $modules | Sort-Object -Property Version -Descending | Select-Object -First 1
            $info = [pscustomobject]([ordered]@{ Available = $true; Version = $top.Version.ToString() })
            New-MeasureResult -Value $info -Display ('available, version ' + $info.Version)
        }
    }
}

function Measure-HyperVServices {
    foreach ($svcName in @('vmms', 'vmcompute')) {
        Invoke-Measure -Id "svc.$svcName" -What "Service $svcName state" `
            -Method "Get-Service -Name $svcName" -Probe {
            $svc = Get-Service -Name $svcName -ErrorAction SilentlyContinue
            if ($null -eq $svc) {
                $info = [pscustomobject]([ordered]@{ Present = $false; Status = 'NOT PRESENT'; StartType = $null })
                New-MeasureResult -Value $info -Display 'service NOT PRESENT (Get-Service found none)'
            }
            else {
                $startType = Get-PropValue -Object $svc -Name 'StartType'
                $info = [pscustomobject]([ordered]@{ Present = $true; Status = [string]$svc.Status; StartType = [string]$startType })
                New-MeasureResult -Value $info -Display ("{0} (start type {1})" -f $info.Status, $info.StartType)
            }
        }
    }
}

function Measure-HyperVInventory {
    Invoke-Measure -Id 'hv.vms' -What 'Existing virtual machines' -Method 'Import-Module Hyper-V; Get-VM' -Probe {
        if (-not (Test-CanQueryHyperV)) { throw 'NEEDS_ADMIN' }
        Import-Module -Name 'Hyper-V' -ErrorAction Stop
        $vms = @(Get-VM)
        $rows = @(foreach ($vm in $vms) {
            [pscustomobject]([ordered]@{
                Name                 = [string]$vm.Name
                State                = [string]$vm.State
                MemoryAssignedGB     = [math]::Round(([double]$vm.MemoryAssigned) / 1GB, 2)
                Generation           = $vm.Generation
                AutomaticStartAction = [string]$vm.AutomaticStartAction
            })
        })
        if ($rows.Count -eq 0) {
            New-MeasureResult -Value $rows -Display 'no VMs (Get-VM returned 0 objects)'
        }
        else {
            $lines = @(foreach ($row in $rows) {
                "{0} | state {1} | memory assigned {2} GB | generation {3} | AutomaticStartAction {4}" -f `
                    $row.Name, $row.State, (Format-GB -Gb $row.MemoryAssignedGB), $row.Generation, $row.AutomaticStartAction
            })
            New-MeasureResult -Value $rows -Display ($lines -join "`n")
        }
    }
    Invoke-Measure -Id 'hv.switches' -What 'Existing virtual switches' -Method 'Import-Module Hyper-V; Get-VMSwitch' -Probe {
        if (-not (Test-CanQueryHyperV)) { throw 'NEEDS_ADMIN' }
        Import-Module -Name 'Hyper-V' -ErrorAction Stop
        $switches = @(Get-VMSwitch)
        $rows = @(foreach ($sw in $switches) {
            [pscustomobject]([ordered]@{ Name = [string]$sw.Name; SwitchType = [string]$sw.SwitchType })
        })
        if ($rows.Count -eq 0) {
            New-MeasureResult -Value $rows -Display 'no switches (Get-VMSwitch returned 0 objects)'
        }
        else {
            $lines = @(foreach ($row in $rows) { "{0} ({1})" -f $row.Name, $row.SwitchType })
            New-MeasureResult -Value $rows -Display ($lines -join "`n")
        }
    }
    if (Test-Measured -Id 'hv.switches') {
        $names = @($script:Index['hv.switches'].Value | ForEach-Object { $_.Name })
        $exists = ($names -contains 'Default Switch')
        Add-Derived -Id 'hv.default_switch' -What "Switch named 'Default Switch' exists" -Rule 'name list contains "Default Switch"' `
            -From @('hv.switches') -Value $exists -Display ("Default Switch exists: $exists")
    }
    else {
        Add-Derived -Id 'hv.default_switch' -What "Switch named 'Default Switch' exists" -Rule 'name list contains "Default Switch"' `
            -From @('hv.switches') -Value $null -NotMeasuredReason 'switch list was not measured'
    }
}

function Measure-Memory {
    Invoke-Measure -Id 'ram.os' -What 'Physical RAM total and free (right now)' `
        -Method 'Get-CimInstance Win32_OperatingSystem (TotalVisibleMemorySize, FreePhysicalMemory; KB converted to GB)' -Probe {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $totalGb = [math]::Round(([double]$os.TotalVisibleMemorySize) / 1MB, 3)
        $freeGb = [math]::Round(([double]$os.FreePhysicalMemory) / 1MB, 3)
        $info = [pscustomobject]([ordered]@{ TotalGB = $totalGb; FreeGB = $freeGb })
        New-MeasureResult -Value $info -Display ("total {0} GB, free {1} GB (free RAM changes second by second)" -f (Format-GB -Gb $totalGb), (Format-GB -Gb $freeGb))
    }
    if (-not (Test-Measured -Id 'ram.os')) {
        Add-Derived -Id 'ram.recommendation' -What 'Recommended VM memory' -Rule 'free RAM minus 4 GB host headroom: >=8 -> 8 GB, >=6 -> 6 GB, else not enough' `
            -From @('ram.os') -Value $null -NotMeasuredReason 'RAM was not measured'
        return
    }
    $free = [double]$script:Index['ram.os'].Value.FreeGB
    $available = $free - $script:HostHeadroomGB
    if ($available -ge 8) {
        $recommended = 8
        $message = 'Recommend an 8 GB VM'
    }
    elseif ($available -ge 6) {
        $recommended = 6
        $message = 'Recommend a 6 GB VM'
    }
    else {
        $recommended = 0
        $message = 'NOT ENOUGH FREE RAM for the pilot right now (needs 6 GB free + 4 GB host headroom)'
    }
    $info = [pscustomobject]([ordered]@{
        FreeGB = $free; HeadroomGB = $script:HostHeadroomGB; AvailableForVmGB = [math]::Round($available, 3)
        RecommendedVmGB = $recommended; Message = $message
    })
    $text = "free {0} GB - {1} GB host headroom = {2} GB for the VM -> {3}" -f (Format-GB -Gb $free), $script:HostHeadroomGB, (Format-GB -Gb $available), $message
    Add-Derived -Id 'ram.recommendation' -What 'Recommended VM memory' -Rule 'free RAM minus 4 GB host headroom: >=8 -> 8 GB, >=6 -> 6 GB, else not enough' `
        -From @('ram.os') -Value $info -Display $text
}

function Measure-Disks {
    Invoke-Measure -Id 'disk.fixed' -What 'Fixed drives: size and free space' `
        -Method 'Get-CimInstance Win32_LogicalDisk -Filter "DriveType = 3" (Size, FreeSpace; bytes converted to GB)' -Probe {
        $systemDrive = $env:SystemDrive
        if (-not $systemDrive) { $systemDrive = 'C:' }
        $disks = @(Get-CimInstance -ClassName Win32_LogicalDisk -Filter 'DriveType = 3')
        $rows = @(foreach ($d in $disks) {
            $sizeGb = $null
            $freeGb = $null
            $pct = $null
            if ($null -ne $d.Size -and [double]$d.Size -gt 0) { $sizeGb = [math]::Round(([double]$d.Size) / 1GB, 3) }
            if ($null -ne $d.FreeSpace) { $freeGb = [math]::Round(([double]$d.FreeSpace) / 1GB, 3) }
            if ($null -ne $sizeGb -and $null -ne $freeGb) { $pct = [math]::Round(100.0 * $freeGb / $sizeGb, 1) }
            [pscustomobject]([ordered]@{
                Drive         = [string]$d.DeviceID
                Label         = [string]$d.VolumeName
                FileSystem    = [string]$d.FileSystem
                SizeGB        = $sizeGb
                FreeGB        = $freeGb
                FreePercent   = $pct
                IsSystemDrive = ([string]$d.DeviceID -eq $systemDrive)
            })
        })
        if ($rows.Count -eq 0) { throw 'REASON: Win32_LogicalDisk returned no fixed drives' }
        $lines = @(foreach ($row in $rows) {
            $flag = ''
            if ($row.IsSystemDrive) { $flag = '  <-- SYSTEM DRIVE (avoid for the VHDX)' }
            "{0} {1} | {2} GB total | {3} GB free ({4}%) | {5}{6}" -f $row.Drive, $row.Label, (Format-GB -Gb $row.SizeGB), (Format-GB -Gb $row.FreeGB), $row.FreePercent, $row.FileSystem, $flag
        })
        New-MeasureResult -Value $rows -Display ($lines -join "`n")
    }
    if (-not (Test-Measured -Id 'disk.fixed')) {
        Add-Derived -Id 'disk.recommendation' -What 'Recommended drive for the VHDX' -Rule 'non-system NTFS/ReFS fixed drive with the most free space, needs >= 100 GB free' `
            -From @('disk.fixed') -Value $null -NotMeasuredReason 'disks were not measured'
        return
    }
    $rows = $script:Index['disk.fixed'].Value
    $candidates = @($rows | Where-Object {
        (-not $_.IsSystemDrive) -and ($null -ne $_.FreeGB) -and ($_.FreeGB -ge 100) -and ($_.FileSystem -in @('NTFS', 'ReFS'))
    })
    if ($candidates.Count -gt 0) {
        $best = $candidates | Sort-Object -Property FreeGB -Descending | Select-Object -First 1
        $info = [pscustomobject]([ordered]@{ RecommendedDrive = $best.Drive; FreeGB = $best.FreeGB; Qualifies = $true })
        $text = "put the VHDX on {0} ({1} GB free, the most free space among non-system fixed drives with >= 100 GB)" -f $best.Drive, (Format-GB -Gb $best.FreeGB)
    }
    else {
        $info = [pscustomobject]([ordered]@{ RecommendedDrive = $null; FreeGB = $null; Qualifies = $false })
        $text = 'no drive qualifies (no fixed NTFS/ReFS drive other than the system drive has >= 100 GB free)'
    }
    Add-Derived -Id 'disk.recommendation' -What 'Recommended drive for the VHDX' -Rule 'non-system NTFS/ReFS fixed drive with the most free space, needs >= 100 GB free' `
        -From @('disk.fixed') -Value $info -Display $text
}

function Join-PathSafe {
    # Like Join-Path, but returns $null instead of throwing when the base is empty.
    param([string]$Base, [string]$Child)
    if ([string]::IsNullOrWhiteSpace($Base)) { return $null }
    return (Join-Path -Path $Base -ChildPath $Child)
}

function Measure-OtherHypervisors {
    $pf = $env:ProgramFiles
    $pf86 = [System.Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    $probes = @(
        @{ Id = 'other.virtualbox'; What = 'VirtualBox'; Pattern = 'VirtualBox';
           Paths = @((Join-PathSafe -Base $pf -Child 'Oracle\VirtualBox\VBoxManage.exe'), 'HKLM:\SOFTWARE\Oracle\VirtualBox') },
        @{ Id = 'other.vmware'; What = 'VMware Workstation / Player'; Pattern = '^VMware (Workstation|Player)';
           Paths = @((Join-PathSafe -Base $pf86 -Child 'VMware\VMware Workstation\vmware.exe'), (Join-PathSafe -Base $pf -Child 'VMware\VMware Workstation\vmware.exe')) },
        @{ Id = 'other.docker'; What = 'Docker Desktop'; Pattern = '^Docker Desktop';
           Paths = @((Join-PathSafe -Base $pf -Child 'Docker\Docker\Docker Desktop.exe')) }
    )
    foreach ($p in $probes) {
        $pattern = $p.Pattern
        $paths = $p.Paths
        $label = $p.What
        Invoke-Measure -Id $p.Id -What "$label installed" `
            -Method 'registry Uninstall keys (HKLM, HKLM Wow6432Node, HKCU) + Program Files path/registry-key existence' -Probe {
            $evidence = @(Get-PresenceEvidence -NamePattern $pattern -Paths $paths)
            if ($evidence.Count -gt 0) {
                $info = [pscustomobject]([ordered]@{ Present = $true; Evidence = $evidence })
                New-MeasureResult -Value $info -Display ("installed (presence only; not proof it works): " + ($evidence -join ' ; '))
            }
            else {
                $info = [pscustomobject]([ordered]@{ Present = $false; Evidence = @() })
                New-MeasureResult -Value $info -Display 'not found by the methods listed below'
            }
        }
    }
    Invoke-Measure -Id 'other.wsl' -What 'WSL status (first lines)' `
        -Method 'wsl.exe --status and wsl.exe --version (first lines of output)' -Probe {
        $cmd = Get-Command -Name 'wsl.exe' -ErrorAction SilentlyContinue
        if ($null -eq $cmd) {
            $info = [pscustomobject]([ordered]@{ Present = $false; Status = @(); Version = @() })
            return (New-MeasureResult -Value $info -Display 'wsl.exe not found on PATH (Get-Command)')
        }
        $statusLines = @()
        $versionLines = @()
        $cap = Invoke-NativeCapture -FilePath $cmd.Source -Arguments '--status' -TimeoutSec 20
        $statusLines = @(($cap.StdOut + "`n" + $cap.StdErr) -split "\r?\n" | ForEach-Object { $_.Trim() } | Where-Object { $_ } | Select-Object -First 6)
        try {
            $cap2 = Invoke-NativeCapture -FilePath $cmd.Source -Arguments '--version' -TimeoutSec 20
            $versionLines = @($cap2.StdOut -split "\r?\n" | ForEach-Object { $_.Trim() } | Where-Object { $_ } | Select-Object -First 3)
        }
        catch {
            $versionLines = @('wsl --version failed: ' + (Limit-Text -Text $_.Exception.Message -Max 80))
        }
        $info = [pscustomobject]([ordered]@{ Present = $true; Status = $statusLines; Version = $versionLines })
        $text = "wsl.exe --status: " + ($statusLines -join ' | ') + "`nwsl.exe --version: " + ($versionLines -join ' | ')
        New-MeasureResult -Value $info -Display $text
    }
    Write-Note 'VirtualBox and VMware Workstation can co-exist with Hyper-V only in a slower mode (they run on top of the Windows Hypervisor Platform instead of their own drivers).'
}

function Measure-Gpu {
    Invoke-Measure -Id 'gpu.adapters' -What 'Display adapters' `
        -Method 'Get-CimInstance Win32_VideoController (Name, AdapterRAM)' -Probe {
        $gpus = @(Get-CimInstance -ClassName Win32_VideoController)
        $rows = @(foreach ($g in $gpus) {
            $ramGb = $null
            if ($null -ne $g.AdapterRAM -and [double]$g.AdapterRAM -gt 0) { $ramGb = [math]::Round(([double]$g.AdapterRAM) / 1GB, 2) }
            [pscustomobject]([ordered]@{ Name = [string]$g.Name; AdapterRAMGB = $ramGb; DriverVersion = [string]$g.DriverVersion })
        })
        if ($rows.Count -eq 0) { throw 'REASON: Win32_VideoController returned no adapters' }
        $lines = @(foreach ($row in $rows) {
            $ramText = 'not reported'
            if ($null -ne $row.AdapterRAMGB) { $ramText = (Format-GB -Gb $row.AdapterRAMGB) + ' GB' }
            "{0} | AdapterRAM {1} | driver {2}" -f $row.Name, $ramText, $row.DriverVersion
        })
        New-MeasureResult -Value $rows -Display ($lines -join "`n")
    }
    Write-Note 'AdapterRAM from WMI is a 32-bit field and under-reports GPUs with more than 4 GB.'
    Write-Note 'GPU partitioning is not supported on Windows 11 Pro Hyper-V; guest rendering is software.'
}

function Write-LicensingNote {
    Write-Section 'DECISION NEEDED BY OWNER - Windows licence for the guest'
    Write-ReportLine 'A Windows guest VM needs its own Windows licence; the licence of this host does not cover it.'
    Write-ReportLine 'Zero-cost pilot path: a 90-day Windows 11 Enterprise evaluation ISO.'
    Write-ReportLine 'This is a decision, not a measurement. It does not change the verdict below.'
}

# ---------------------------------------------------------------------------
# Verdict
# ---------------------------------------------------------------------------
function Add-Finding {
    param(
        [Parameter(Mandatory)][ValidateSet('Blocker', 'Condition', 'Passed')][string]$Level,
        [Parameter(Mandatory)][string]$Text,
        [string[]]$Ids = @()
    )
    $cites = @($Ids | ForEach-Object { Get-Cite -Id $_ })
    $script:Findings.Add([pscustomobject]([ordered]@{ Level = $Level; Text = $Text; Evidence = @($Ids); Cites = $cites }))
}

function Test-EditionFinding {
    $ids = @('os.info', 'os.hyperv_edition')
    $support = Get-MeasuredScalar -Id 'os.hyperv_edition'
    if ($null -eq $support) { Add-Finding -Level Blocker -Text 'Windows edition could not be measured, so readiness cannot be judged.' -Ids $ids; return }
    if ($support -eq 'YES') { Add-Finding -Level Passed -Text 'Windows edition supports Hyper-V.' -Ids $ids }
    elseif ($support -eq 'NO') { Add-Finding -Level Blocker -Text 'This Windows edition (Home) cannot run Hyper-V. Upgrade to Pro/Enterprise/Education.' -Ids $ids }
    else { Add-Finding -Level Condition -Text 'Windows edition is not recognised as Hyper-V capable; check it manually.' -Ids $ids }
}

function Test-CpuFinding {
    $hvp = Get-MeasuredScalar -Id 'cpu.hypervisor_present'
    if ($hvp -eq $true) {
        Add-Finding -Level Passed -Text 'A hypervisor is already running, so CPU virtualisation is evidently active.' -Ids @('cpu.hypervisor_present')
        return
    }
    $checks = @(
        @{ Id = 'cpu.virt_fw'; Text = 'Virtualisation is switched off in firmware (enable Intel VT-x / AMD-V in BIOS/UEFI)'; Okay = 'Virtualisation is enabled in firmware.' },
        @{ Id = 'cpu.slat';    Text = 'CPU lacks Second Level Address Translation (Hyper-V cannot run)'; Okay = 'CPU reports Second Level Address Translation.' },
        @{ Id = 'cpu.vmmode';  Text = 'CPU lacks VM Monitor Mode Extensions (Hyper-V cannot run)'; Okay = 'CPU reports VM Monitor Mode Extensions.' }
    )
    foreach ($c in $checks) {
        $value = Get-MeasuredScalar -Id $c.Id
        $ids = @($c.Id, 'cpu.hypervisor_present')
        if ($null -eq $value) {
            Add-Finding -Level Condition -Text ("Could not measure: " + $c.Id) -Ids $ids
        }
        elseif ($value -eq $false) {
            # Without a measured "no hypervisor running" the False could be a side effect of a running hypervisor.
            if ($hvp -eq $false) { Add-Finding -Level Blocker -Text $c.Text -Ids $ids }
            else { Add-Finding -Level Condition -Text ($c.Text + ' (reported False, but hypervisor presence was not measured)') -Ids $ids }
        }
        else {
            Add-Finding -Level Passed -Text $c.Okay -Ids $ids
        }
    }
}

function Test-HyperVFeatureFinding {
    $feature = Get-MeasuredScalar -Id 'hv.feature'
    $vmms = $null
    if (Test-Measured -Id 'svc.vmms') { $vmms = $script:Index['svc.vmms'].Value }
    $vmmsRunning = ($null -ne $vmms -and $vmms.Present -and $vmms.Status -eq 'Running')
    if ($null -eq $feature) {
        if ($vmmsRunning) {
            Add-Finding -Level Passed -Text 'Feature state not measured (needs admin), but the Hyper-V management service vmms is Running.' -Ids @('hv.feature', 'svc.vmms')
        }
        else {
            Add-Finding -Level Condition -Text 'Hyper-V feature state not measured. Re-run this script from an elevated PowerShell.' -Ids @('hv.feature')
        }
    }
    elseif ($feature -eq 'Enabled') {
        Add-Finding -Level Passed -Text 'Hyper-V optional feature reports Enabled.' -Ids @('hv.feature')
    }
    else {
        Add-Finding -Level Condition -Text "Hyper-V feature is '$feature'. It must be enabled by an administrator (then reboot) before Stage 1." -Ids @('hv.feature')
    }
}

function Test-HyperVModuleFinding {
    if (-not (Test-Measured -Id 'hv.module')) {
        Add-Finding -Level Condition -Text 'Hyper-V PowerShell module availability not measured.' -Ids @('hv.module')
        return
    }
    if ($script:Index['hv.module'].Value.Available) {
        Add-Finding -Level Passed -Text 'Hyper-V PowerShell module is available.' -Ids @('hv.module')
    }
    else {
        Add-Finding -Level Condition -Text 'Hyper-V PowerShell module is missing (enable Hyper-V Management Tools / Microsoft-Hyper-V-Management-PowerShell).' -Ids @('hv.module')
    }
}

function Test-ServiceFinding {
    if (-not (Test-Measured -Id 'svc.vmms')) {
        Add-Finding -Level Condition -Text 'Service vmms state not measured.' -Ids @('svc.vmms')
        return
    }
    $svc = $script:Index['svc.vmms'].Value
    if (-not $svc.Present) {
        Add-Finding -Level Condition -Text 'Service vmms (Hyper-V Virtual Machine Management) is not present; Hyper-V is not installed.' -Ids @('svc.vmms')
    }
    elseif ($svc.Status -ne 'Running') {
        Add-Finding -Level Condition -Text ("Service vmms is '" + $svc.Status + "'; it must be Running (reboot after enabling Hyper-V).") -Ids @('svc.vmms')
    }
    else {
        Add-Finding -Level Passed -Text 'Service vmms is Running.' -Ids @('svc.vmms')
    }
}

function Test-SwitchFinding {
    $exists = Get-MeasuredScalar -Id 'hv.default_switch'
    if ($null -eq $exists) {
        Add-Finding -Level Condition -Text 'Virtual switches not measured (needs admin or Hyper-V Administrators membership).' -Ids @('hv.switches')
    }
    elseif ($exists) {
        Add-Finding -Level Passed -Text "'Default Switch' exists." -Ids @('hv.switches', 'hv.default_switch')
    }
    else {
        Add-Finding -Level Condition -Text "'Default Switch' does not exist; New-Workbench.ps1 would need -SwitchName with a switch that does." -Ids @('hv.switches', 'hv.default_switch')
    }
}

function Test-NameClashFinding {
    if (-not (Test-Measured -Id 'hv.vms')) { return }
    $names = @($script:Index['hv.vms'].Value | ForEach-Object { $_.Name })
    if ($names -contains $script:DefaultVmName) {
        Add-Finding -Level Condition -Text ("A VM named '" + $script:DefaultVmName + "' already exists; New-Workbench.ps1 will refuse unless you pass another -Name.") -Ids @('hv.vms')
    }
}

function Test-MemoryFinding {
    if (-not (Test-Measured -Id 'ram.recommendation')) {
        Add-Finding -Level Blocker -Text 'RAM could not be measured, so readiness cannot be judged.' -Ids @('ram.os')
        return
    }
    $rec = $script:Index['ram.recommendation'].Value
    if ($rec.RecommendedVmGB -eq 0) {
        Add-Finding -Level Blocker -Text 'NOT ENOUGH FREE RAM for the pilot right now (needs 6 GB free + 4 GB host headroom). Close programs and run again.' -Ids @('ram.os', 'ram.recommendation')
    }
    else {
        Add-Finding -Level Passed -Text ("Enough free RAM right now for a VM of " + $rec.RecommendedVmGB + " GB.") -Ids @('ram.os', 'ram.recommendation')
    }
}

function Test-DiskFinding {
    if (-not (Test-Measured -Id 'disk.fixed')) {
        Add-Finding -Level Blocker -Text 'Disks could not be measured, so readiness cannot be judged.' -Ids @('disk.fixed')
        return
    }
    $rows = $script:Index['disk.fixed'].Value
    $roomy = @($rows | Where-Object { $null -ne $_.FreeGB -and $_.FreeGB -ge 60 })
    $rec = $script:Index['disk.recommendation'].Value
    if ($roomy.Count -eq 0) {
        Add-Finding -Level Blocker -Text 'No fixed drive has 60 GB free, which New-Workbench.ps1 requires before creating the dynamic VHDX.' -Ids @('disk.fixed')
    }
    elseif ($rec.Qualifies) {
        Add-Finding -Level Passed -Text ("Drive " + $rec.RecommendedDrive + " has room for the VHDX.") -Ids @('disk.fixed', 'disk.recommendation')
    }
    else {
        Add-Finding -Level Condition -Text 'No non-system drive has >= 100 GB free. The VHDX would have to go on the system drive (needs -AllowSystemDrive) or you add storage.' -Ids @('disk.fixed', 'disk.recommendation')
    }
}

function Get-Verdict {
    $script:Findings.Clear()
    Test-EditionFinding
    Test-CpuFinding
    Test-HyperVFeatureFinding
    Test-HyperVModuleFinding
    Test-ServiceFinding
    Test-SwitchFinding
    Test-NameClashFinding
    Test-MemoryFinding
    Test-DiskFinding
    $blockers = @($script:Findings | Where-Object { $_.Level -eq 'Blocker' })
    $conditions = @($script:Findings | Where-Object { $_.Level -eq 'Condition' })
    $passed = @($script:Findings | Where-Object { $_.Level -eq 'Passed' })
    if ($blockers.Count -gt 0) { $level = 'NOT READY' }
    elseif ($conditions.Count -gt 0) { $level = 'READY WITH CONDITIONS' }
    else { $level = 'READY FOR STAGE 1' }
    return [pscustomobject]([ordered]@{ Level = $level; Blockers = $blockers; Conditions = $conditions; Passed = $passed })
}

function Write-FindingList {
    param([string]$Heading, $Items, [string]$Color)
    Write-ReportLine ''
    Write-ReportLine $Heading $Color
    if (@($Items).Count -eq 0) { Write-ReportLine '  (none)'; return }
    foreach ($item in @($Items)) {
        Write-ReportLine ('  - ' + $item.Text) $Color
        foreach ($cite in $item.Cites) { Write-ReportLine ('      evidence ' + $cite) 'DarkGray' }
    }
}

function Write-Verdict {
    param($Verdict)
    $color = 'Yellow'
    if ($Verdict.Level -eq 'NOT READY') { $color = 'Red' }
    elseif ($Verdict.Level -eq 'READY FOR STAGE 1') { $color = 'Green' }
    Write-ReportLine ''
    Write-ReportLine ('=' * 72)
    Write-ReportLine ('VERDICT: ' + $Verdict.Level) $color
    Write-ReportLine ('=' * 72)
    Write-FindingList -Heading 'Blockers:' -Items $Verdict.Blockers -Color 'Red'
    Write-FindingList -Heading 'Conditions:' -Items $Verdict.Conditions -Color 'Yellow'
    Write-FindingList -Heading 'Checks that passed (each with its measurement):' -Items $Verdict.Passed -Color ''
    Write-ReportLine ''
    Write-ReportLine 'Not part of the verdict: the Windows licence for the guest is a DECISION NEEDED BY OWNER (see above).'
}

# ---------------------------------------------------------------------------
# Report files
# ---------------------------------------------------------------------------
function Get-ReportDirectory {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
    return (Join-Path -Path $env:LOCALAPPDATA -ChildPath 'Avni\reports')
}

function Get-FreeReportBase {
    # Picks a base path that does not exist yet, so an earlier report is never overwritten.
    param([string]$Directory, [string]$Prefix)
    $stamp = $script:RunStarted.ToString('dd-MM-yyyy_HHmm', $script:Culture)
    $base = Join-Path -Path $Directory -ChildPath ($Prefix + '-' + $stamp)
    $candidate = $base
    $n = 1
    while ((Test-Path -LiteralPath ($candidate + '.json')) -or (Test-Path -LiteralPath ($candidate + '.txt'))) {
        $n++
        $candidate = $base + '-' + $n
    }
    return $candidate
}

function Save-Report {
    param($Verdict)
    $dir = Get-ReportDirectory
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $base = Get-FreeReportBase -Directory $dir -Prefix 'workbench-preflight'
    $report = [ordered]@{
        Tool            = $script:ToolName
        ToolVersion     = $script:ToolVersion
        GeneratedAt     = $script:RunStarted.ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
        PowerShell      = $PSVersionTable.PSVersion.ToString()
        RanElevated     = $script:IsAdmin
        Verdict         = $Verdict
        Notes           = $script:Notes.ToArray()
        OwnerDecision   = 'Windows licence for the guest VM (90-day Windows 11 Enterprise evaluation ISO is the zero-cost pilot path)'
        Measurements    = $script:Records.ToArray()
    }
    $json = ConvertTo-Json -InputObject $report -Depth 8
    $utf8 = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText(($base + '.json'), $json, $utf8)
    [System.IO.File]::WriteAllText(($base + '.txt'), ($script:Lines -join [System.Environment]::NewLine), $utf8)
    return $base
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
function Invoke-Section {
    # One section failing (a bug, not a missing feature) must not stop the other sections.
    param([string]$Title, [scriptblock]$Body)
    Write-Section $Title
    try { & $Body }
    catch { Write-ReportLine ("INTERNAL ERROR in section '" + $Title + "': " + $_.Exception.Message) 'Red' }
}

function Invoke-Preflight {
    $script:IsAdmin = Test-IsAdmin
    Write-ReportLine ('AVNI Agent Workbench - host preflight (read-only) - ' + (Get-Stamp)) 'White'
    Write-ReportLine 'Every line below is MEASURED (value + method + time), DERIVED (rule + inputs) or NOT MEASURED. Nothing is inferred silently.'

    Invoke-Section 'Host' { Measure-HostIdentity }
    Invoke-Section 'Windows' { Measure-WindowsEdition }
    Invoke-Section 'CPU and hypervisor' { Measure-Cpu; Measure-SystemInfo }
    Invoke-Section 'Hyper-V feature, module, services' { Measure-HyperVFeature; Measure-HyperVServices }
    Invoke-Section 'Hyper-V inventory (VMs, switches)' { Measure-HyperVInventory }
    Invoke-Section 'Memory' { Measure-Memory }
    Invoke-Section 'Disks' { Measure-Disks }
    Invoke-Section 'Other virtualisation software' { Measure-OtherHypervisors }
    Invoke-Section 'GPU' { Measure-Gpu }
    Write-LicensingNote

    $script:Verdict = Get-Verdict
    Write-Verdict -Verdict $script:Verdict
}

try {
    Invoke-Preflight
}
catch {
    # Last-resort guard: a bug in the report logic itself must still end in a clear message.
    Write-ReportLine ('INTERNAL ERROR while building the report: ' + $_.Exception.Message) 'Red'
}

try {
    $reportBase = Save-Report -Verdict $script:Verdict
    Write-Host ''
    Write-Host ('Report written: ' + $reportBase + '.json') -ForegroundColor White
    Write-Host ('Report written: ' + $reportBase + '.txt') -ForegroundColor White
    exit 0
}
catch {
    Write-Host ''
    Write-Host ('COULD NOT WRITE THE REPORT: ' + $_.Exception.Message) -ForegroundColor Red
    exit 2
}
