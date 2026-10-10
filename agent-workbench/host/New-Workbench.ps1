#Requires -Version 5.1
<#
.SYNOPSIS
Creates the (powered-off) Hyper-V virtual machine for the AVNI Agent Workbench.

.DESCRIPTION
This script builds ONE Generation 2 Hyper-V VM with a dynamic virtual disk, a
virtual TPM and Secure Boot (what Windows 11 needs), then reads the VM back from
Hyper-V and prints what Hyper-V actually reports. It needs an elevated
(administrator) PowerShell.

What it does:
  - Checks first (and prints each check with its value, method and time):
    admin rights, Hyper-V module, no VM of that name yet, the VHDX folder is an
    existing directory on a local fixed drive (not C: unless you allow it),
    at least 60 GB really free, the VHDX file does not exist yet, the ISO
    exists (if given), the virtual switch exists.
  - Creates: the VHDX (dynamic), the VM, memory (dynamic), processors, Secure
    Boot, virtual TPM, the Guest Service Interface (so the host can copy files
    in later), optional 1920x1080 video, and the Windows ISO as first boot device.
  - Turns on Enhanced Session Mode on the host (a host-wide Hyper-V setting).
  - Sets AutomaticStartAction = Nothing so the VM NEVER starts when the host boots.
  - Writes a JSON record to %LOCALAPPDATA%\Avni\reports\workbench-create-dd-MM-yyyy_HHmm.json
  - Prints numbered next steps for a human, one action per line.

What it never does:
  - It never starts the VM and never runs Windows setup. The VM stays Off.
  - It never deletes anything: no VM, no disk, no folder. If something goes wrong
    half way it stops and tells you exactly what exists; you decide what to remove.
  - It never overwrites an existing VM name or an existing VHDX file.
  - It never auto-elevates; if you are not an administrator it refuses.
  - It never writes to Desktop, Documents or project folders (the VM files go to
    the folder you give in -VhdRoot; the record goes to %LOCALAPPDATA%\Avni\reports).
  - It never touches AutoHotkey or any other running program.
  - It contains no secrets and downloads nothing.

Files created: <VhdRoot>\<Name>\ (VM configuration, checkpoints) and
<VhdRoot>\<Name>\Virtual Hard Disks\<Name>.vhdx. Keeping everything under one
folder on the chosen drive means the VM and its disk survive a host reboot and
checkpoints do not fill the system drive.

Exit codes: 0 created and left powered off; 1 refused (nothing created);
3 failed part-way (something may exist; see the printed list); 2 could not write the record.

.PARAMETER Name
VM name. Letters, digits, space, dot, underscore, hyphen. Default 'AVNI Agent Workbench 01'.

.PARAMETER VhdRoot
REQUIRED. An existing folder on a local fixed NTFS/ReFS drive, for example D:\HyperV.
C: (the system drive) is refused unless you also pass -AllowSystemDrive.

.PARAMETER VhdSizeGB
Maximum size of the dynamic disk. It only uses real space as Windows fills it. Default 200.

.PARAMETER MemoryStartupGB
Memory the VM starts with. Default 6.

.PARAMETER MemoryMaxGB
Upper limit for dynamic memory. Default 8.

.PARAMETER ProcessorCount
Virtual processors. Default 4.

.PARAMETER IsoPath
Optional Windows installation ISO. If given it must exist; it is attached as a DVD
and set as the first boot device.

.PARAMETER SwitchName
Virtual switch to connect to. Default 'Default Switch'.

.PARAMETER AllowSystemDrive
Allow -VhdRoot to be on the system drive (C:). Not recommended.

.EXAMPLE
.\New-Workbench.ps1 -VhdRoot D:\HyperV -IsoPath D:\ISO\Win11_Enterprise_Eval.iso
Creates the VM with the ISO attached. Does not start it.

.EXAMPLE
.\New-Workbench.ps1 -VhdRoot D:\HyperV -WhatIf
Runs every check and says what it would create, without creating anything.
#>
[CmdletBinding(SupportsShouldProcess = $true)]
param(
    [ValidatePattern('^[A-Za-z0-9]([A-Za-z0-9 ._-]{0,62}[A-Za-z0-9])?$')]
    [string]$Name = 'AVNI Agent Workbench 01',

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$VhdRoot,

    [ValidateRange(40, 4000)]
    [int]$VhdSizeGB = 200,

    [ValidateRange(2, 128)]
    [int]$MemoryStartupGB = 6,

    [ValidateRange(2, 256)]
    [int]$MemoryMaxGB = 8,

    [ValidateRange(1, 64)]
    [int]$ProcessorCount = 4,

    [string]$IsoPath,

    [ValidateNotNullOrEmpty()]
    [string]$SwitchName = 'Default Switch',

    [switch]$AllowSystemDrive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Script-wide state
# ---------------------------------------------------------------------------
$script:ToolName     = 'New-Workbench'
$script:ToolVersion  = '1.0.0'
$script:Culture      = [System.Globalization.CultureInfo]::InvariantCulture
$script:Started      = Get-Date
$script:Checks       = New-Object System.Collections.Generic.List[object]
$script:Steps        = New-Object System.Collections.Generic.List[object]
$script:Reported     = New-Object System.Collections.Generic.List[object]
$script:Warnings     = New-Object System.Collections.Generic.List[string]
$script:VhdRootFull  = $null
$script:VhdDir       = $null
$script:VhdPath      = $null
$script:IsoFull      = $null
$script:DriveLetter  = $null
$script:Outcome      = 'NOT_STARTED'

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
    param($Gb)
    if ($null -eq $Gb) { return 'n/a' }
    return ([double]$Gb).ToString('0.0', $script:Culture)
}

function Limit-Text {
    param([string]$Text, [int]$Max = 160)
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

function New-MeasureResult {
    param($Value, [string]$Display = '')
    $result = [pscustomobject]@{ Value = $Value; Display = $Display }
    $result.PSObject.TypeNames.Insert(0, 'Avni.MeasureResult')
    return $result
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

function Invoke-Measure {
    # Runs one read-only probe, prints value + method + time, returns a record.
    # A probe that throws becomes NOT MEASURED; it never stops the script by itself.
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)][string]$What,
        [Parameter(Mandatory)][string]$Method,
        [Parameter(Mandatory)][scriptblock]$Probe,
        [string]$Requested,
        [switch]$IsReport
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
        $mxDisplay = 'NOT MEASURED (' + (Limit-Text -Text $_.Exception.Message -Max 160) + ')'
    }
    $mxMatches = $null
    if ($PSBoundParameters.ContainsKey('Requested') -and $mxStatus -eq 'MEASURED') {
        $mxMatches = ([string]$mxValue -eq $Requested)
        if ($mxMatches) { $mxDisplay = $mxDisplay + ' | requested ' + $Requested + ': matches the request' }
        else { $mxDisplay = $mxDisplay + ' | requested ' + $Requested + ': DIFFERS FROM THE REQUEST' }
    }
    $record = [pscustomobject]([ordered]@{
        What       = $What
        Status     = $mxStatus
        Value      = $mxValue
        Display    = $mxDisplay
        Method     = $Method
        MeasuredAt = $mxAt
        MatchesRequest = $mxMatches
    })
    if ($mxStatus -eq 'MEASURED' -and $mxMatches -ne $false) { $tag = '[MEASURED]'; $color = 'Cyan' }
    elseif ($mxStatus -eq 'MEASURED') { $tag = '[MEASURED]'; $color = 'Red' }
    else { $tag = '[NOT MEASURED]'; $color = 'Yellow' }
    Write-Tagged -Tag $tag -Text ($What + ': ' + $mxDisplay) -Color $color
    Write-Detail ('method: ' + $Method + ' | at ' + $mxAt)
    if ($IsReport) { $script:Reported.Add($record) } else { $script:Checks.Add($record) }
    return $record
}

function Get-ReportDirectory {
    if ([string]::IsNullOrWhiteSpace($env:LOCALAPPDATA)) { throw 'LOCALAPPDATA is not set' }
    return (Join-Path -Path $env:LOCALAPPDATA -ChildPath 'Avni\reports')
}

function Get-FreeReportBase {
    param([string]$Directory, [string]$Prefix)
    $stamp = $script:Started.ToString('dd-MM-yyyy_HHmm', $script:Culture)
    $base = Join-Path -Path $Directory -ChildPath ($Prefix + '-' + $stamp)
    $candidate = $base
    $n = 1
    while (Test-Path -LiteralPath ($candidate + '.json')) {
        $n++
        $candidate = $base + '-' + $n
    }
    return $candidate
}

function Save-CreateRecord {
    # Writes the JSON record. Returns the path, or $null (after printing why) if it failed.
    param([string]$Outcome)
    try {
        $dir = Get-ReportDirectory
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
        $base = Get-FreeReportBase -Directory $dir -Prefix 'workbench-create'
        $record = [ordered]@{
            Tool        = $script:ToolName
            ToolVersion = $script:ToolVersion
            StartedAt   = $script:Started.ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            FinishedAt  = (Get-Date).ToString('yyyy-MM-ddTHH:mm:ssK', $script:Culture)
            Outcome     = $Outcome
            ScriptStartedTheVm = $false
            Parameters  = [ordered]@{
                Name = $Name; VhdRoot = $script:VhdRootFull; VhdPath = $script:VhdPath
                VhdSizeGB = $VhdSizeGB; MemoryStartupGB = $MemoryStartupGB; MemoryMaxGB = $MemoryMaxGB
                ProcessorCount = $ProcessorCount; IsoPath = $script:IsoFull; SwitchName = $SwitchName
                AllowSystemDrive = [bool]$AllowSystemDrive
            }
            Checks      = $script:Checks.ToArray()
            Steps       = $script:Steps.ToArray()
            ReadBackFromHyperV = $script:Reported.ToArray()
            Warnings    = $script:Warnings.ToArray()
        }
        $json = ConvertTo-Json -InputObject $record -Depth 8
        $path = $base + '.json'
        [System.IO.File]::WriteAllText($path, $json, (New-Object System.Text.UTF8Encoding($false)))
        Write-Host ''
        Write-Host ('Record written: ' + $path) -ForegroundColor White
        return $path
    }
    catch {
        Write-Host ''
        Write-Host ('COULD NOT WRITE THE RECORD: ' + $_.Exception.Message) -ForegroundColor Red
        return $null
    }
}

function Stop-Refused {
    # Refusal = nothing was created. Exit code 1.
    param([string]$Reason)
    Write-Host ''
    Write-Host ('REFUSED: ' + $Reason) -ForegroundColor Red
    Write-Host 'Nothing was created and nothing was changed.' -ForegroundColor Red
    exit 1
}

function Add-Warning {
    param([string]$Text)
    $script:Warnings.Add($Text)
    Write-Tagged -Tag '[WARNING]' -Text $Text -Color 'Yellow'
}

# ---------------------------------------------------------------------------
# Pre-flight checks (all read-only)
# ---------------------------------------------------------------------------
function Test-Prerequisites {
    $adminRec = Invoke-Measure -What 'Running elevated (administrator)' -Method 'WindowsPrincipal.IsInRole(Administrator)' -Probe {
        $v = Test-IsAdmin
        New-MeasureResult -Value $v -Display ([string]$v)
    }
    if (-not ($adminRec.Status -eq 'MEASURED' -and $adminRec.Value -eq $true)) {
        Stop-Refused 'Not running as Administrator. Open an elevated PowerShell (Start menu, type PowerShell, right-click Windows PowerShell, Run as administrator) and run this command again.'
    }

    $modRec = Invoke-Measure -What 'Hyper-V PowerShell module available' -Method 'Get-Module -ListAvailable -Name Hyper-V' -Probe {
        $found = @(Get-Module -ListAvailable -Name 'Hyper-V')
        New-MeasureResult -Value ($found.Count -gt 0) -Display ([string]($found.Count -gt 0))
    }
    if (-not ($modRec.Status -eq 'MEASURED' -and $modRec.Value -eq $true)) {
        Stop-Refused 'The Hyper-V PowerShell module is not available. Run Preflight-Workbench.ps1 to see what is missing, enable Hyper-V as an administrator, reboot, then try again.'
    }
    Import-Module -Name 'Hyper-V' -ErrorAction Stop

    $vmRec = Invoke-Measure -What ("A VM named '" + $Name + "' already exists") -Method 'Get-VM -Name (Hyper-V)' -Probe {
        $existing = @(Get-VM -Name $Name -ErrorAction SilentlyContinue)
        if ($existing.Count -gt 0) {
            $state = [string]$existing[0].State
            New-MeasureResult -Value ([pscustomobject]@{ Exists = $true; State = $state }) -Display ("True, current state: " + $state)
        }
        else {
            New-MeasureResult -Value ([pscustomobject]@{ Exists = $false; State = $null }) -Display 'False (Get-VM found none)'
        }
    }
    if ($vmRec.Status -ne 'MEASURED') { Stop-Refused 'Could not read the existing VM list, so cannot be sure the name is free.' }
    if ($vmRec.Value.Exists) {
        Stop-Refused ("A VM named '" + $Name + "' already exists (state: " + $vmRec.Value.State + "). This script never replaces or deletes a VM. Choose another -Name or remove the old VM yourself in Hyper-V Manager.")
    }
}

function Test-StorageTarget {
    $rootRec = Invoke-Measure -What '-VhdRoot is an existing directory' -Method 'Test-Path -PathType Container; Resolve-Path' -Probe {
        if (-not (Test-Path -LiteralPath $VhdRoot -PathType Container)) {
            New-MeasureResult -Value $null -Display ("False (no such directory: " + $VhdRoot + ")")
        }
        else {
            $full = (Resolve-Path -LiteralPath $VhdRoot).ProviderPath
            New-MeasureResult -Value $full -Display ("True: " + $full)
        }
    }
    if ($rootRec.Status -ne 'MEASURED' -or $null -eq $rootRec.Value) {
        Stop-Refused ("-VhdRoot '" + $VhdRoot + "' is not an existing directory. Create the folder first (for example D:\HyperV) and run again.")
    }
    $full = [string]$rootRec.Value
    if ($full -notmatch '^[A-Za-z]:\\') {
        Stop-Refused ("-VhdRoot must be on a local drive letter (found '" + $full + "'). Network paths are not allowed for the VM disk.")
    }
    if ($full.Length -gt 3) { $full = $full.TrimEnd('\') }
    $script:VhdRootFull = $full
    $script:DriveLetter = $full.Substring(0, 2).ToUpperInvariant()

    $diskRec = Invoke-Measure -What ("Drive " + $script:DriveLetter + " type, file system, free space") `
        -Method 'Get-CimInstance Win32_LogicalDisk -Filter DeviceID (DriveType, FileSystem, FreeSpace)' -Probe {
        $disk = Get-CimInstance -ClassName Win32_LogicalDisk -Filter ("DeviceID='" + $script:DriveLetter + "'")
        if ($null -eq $disk) { throw 'drive not found by WMI' }
        $freeGb = [math]::Round(([double]$disk.FreeSpace) / 1GB, 3)
        $info = [pscustomobject]([ordered]@{ DriveType = [int]$disk.DriveType; FileSystem = [string]$disk.FileSystem; FreeGB = $freeGb })
        New-MeasureResult -Value $info -Display ("DriveType {0} (3 = fixed), {1}, {2} GB free" -f $info.DriveType, $info.FileSystem, (Format-GB -Gb $freeGb))
    }
    if ($diskRec.Status -ne 'MEASURED') { Stop-Refused ("Could not measure drive " + $script:DriveLetter + ", so free space is unknown.") }
    if ($diskRec.Value.DriveType -ne 3) { Stop-Refused ("Drive " + $script:DriveLetter + " is not a fixed local disk (DriveType " + $diskRec.Value.DriveType + "). Use a fixed internal drive.") }
    if ($diskRec.Value.FileSystem -notin @('NTFS', 'ReFS')) { Stop-Refused ("Drive " + $script:DriveLetter + " is " + $diskRec.Value.FileSystem + "; use an NTFS or ReFS drive.") }

    $systemDrive = $env:SystemDrive
    if (-not $systemDrive) { $systemDrive = 'C:' }
    if ($script:DriveLetter -eq $systemDrive.ToUpperInvariant() -and -not $AllowSystemDrive) {
        Stop-Refused ("-VhdRoot is on the system drive (" + $script:DriveLetter + "). A growing VM disk can fill Windows itself. Use another drive, or pass -AllowSystemDrive if you accept that.")
    }
    if ([double]$diskRec.Value.FreeGB -lt 60) {
        Stop-Refused ("Only " + (Format-GB -Gb $diskRec.Value.FreeGB) + " GB free on " + $script:DriveLetter + "; at least 60 GB must be actually free (the dynamic disk grows).")
    }
    if ([double]$diskRec.Value.FreeGB -lt $VhdSizeGB) {
        Add-Warning ("Free space (" + (Format-GB -Gb $diskRec.Value.FreeGB) + " GB) is less than the disk's maximum size (" + $VhdSizeGB + " GB). Hyper-V pauses a VM whose disk cannot grow; keep an eye on free space.")
    }

    $script:VhdDir = [System.IO.Path]::Combine($script:VhdRootFull, $Name, 'Virtual Hard Disks')
    $script:VhdPath = [System.IO.Path]::Combine($script:VhdDir, ($Name + '.vhdx'))
    $vhdRec = Invoke-Measure -What 'VHDX file already exists' -Method ('Test-Path -LiteralPath ' + $script:VhdPath) -Probe {
        $exists = Test-Path -LiteralPath $script:VhdPath
        New-MeasureResult -Value $exists -Display ([string]$exists)
    }
    if ($vhdRec.Status -ne 'MEASURED' -or $vhdRec.Value -eq $true) {
        Stop-Refused ("The VHDX path already exists or could not be checked: " + $script:VhdPath + ". This script never overwrites disks.")
    }
}

function Test-OtherInputs {
    if ($MemoryMaxGB -lt $MemoryStartupGB) {
        Stop-Refused ("-MemoryMaxGB (" + $MemoryMaxGB + ") must be at least -MemoryStartupGB (" + $MemoryStartupGB + ").")
    }
    if (-not [string]::IsNullOrWhiteSpace($IsoPath)) {
        $isoRec = Invoke-Measure -What '-IsoPath is an existing file' -Method 'Test-Path -PathType Leaf; Resolve-Path' -Probe {
            if (-not (Test-Path -LiteralPath $IsoPath -PathType Leaf)) {
                New-MeasureResult -Value $null -Display ("False (no such file: " + $IsoPath + ")")
            }
            else {
                $full = (Resolve-Path -LiteralPath $IsoPath).ProviderPath
                New-MeasureResult -Value $full -Display ("True: " + $full)
            }
        }
        if ($isoRec.Status -ne 'MEASURED' -or $null -eq $isoRec.Value) { Stop-Refused ("-IsoPath '" + $IsoPath + "' does not exist.") }
        if ([System.IO.Path]::GetExtension([string]$isoRec.Value).ToLowerInvariant() -ne '.iso') { Stop-Refused '-IsoPath must be a .iso file.' }
        $script:IsoFull = [string]$isoRec.Value
    }
    $swRec = Invoke-Measure -What ("Virtual switch '" + $SwitchName + "' exists") -Method 'Get-VMSwitch -Name' -Probe {
        $found = @(Get-VMSwitch -Name $SwitchName -ErrorAction SilentlyContinue)
        New-MeasureResult -Value ($found.Count -gt 0) -Display ([string]($found.Count -gt 0))
    }
    if ($swRec.Status -ne 'MEASURED' -or $swRec.Value -ne $true) {
        Stop-Refused ("Virtual switch '" + $SwitchName + "' was not found. List switches with Get-VMSwitch and pass the right -SwitchName.")
    }
    $ramRec = Invoke-Measure -What 'Host RAM free right now' -Method 'Get-CimInstance Win32_OperatingSystem (FreePhysicalMemory)' -Probe {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $freeGb = [math]::Round(([double]$os.FreePhysicalMemory) / 1MB, 3)
        New-MeasureResult -Value $freeGb -Display ((Format-GB -Gb $freeGb) + ' GB')
    }
    if ($ramRec.Status -eq 'MEASURED' -and ([double]$ramRec.Value - 4) -lt $MemoryStartupGB) {
        Add-Warning ("Host has " + (Format-GB -Gb $ramRec.Value) + " GB free RAM; with 4 GB kept for the host that is less than the VM's startup memory (" + $MemoryStartupGB + " GB). Creating the VM is fine; starting it may be slow until you free memory.")
    }
}

# ---------------------------------------------------------------------------
# Creation
# ---------------------------------------------------------------------------
function Invoke-Step {
    # Runs one creation action. Prints a line saying the cmdlet returned without error
    # (that is NOT the proof; the read-back below is). Returns $true/$false.
    param(
        [Parameter(Mandatory)][string]$Title,
        [Parameter(Mandatory)][string]$Command,
        [Parameter(Mandatory)][scriptblock]$Action,
        [switch]$Optional
    )
    $at = Get-Stamp
    try {
        $null = & $Action
        $script:Steps.Add([pscustomobject]([ordered]@{ Step = $Title; Command = $Command; Result = 'cmdlet returned without error'; At = $at }))
        Write-Tagged -Tag '[ACTION DONE]' -Text ($Title + ' (cmdlet returned without error)') -Color 'Cyan'
        Write-Detail ('command: ' + $Command + ' | at ' + $at)
        return $true
    }
    catch {
        $msg = Limit-Text -Text $_.Exception.Message -Max 300
        if ($Optional) {
            $script:Steps.Add([pscustomobject]([ordered]@{ Step = $Title; Command = $Command; Result = ('OPTIONAL STEP FAILED: ' + $msg); At = $at }))
            Write-Tagged -Tag '[OPTIONAL FAILED]' -Text ($Title + ': ' + $msg) -Color 'Yellow'
            $script:Warnings.Add($Title + ' failed: ' + $msg)
        }
        else {
            $script:Steps.Add([pscustomobject]([ordered]@{ Step = $Title; Command = $Command; Result = ('FAILED: ' + $msg); At = $at }))
            Write-Tagged -Tag '[FAILED]' -Text ($Title + ': ' + $msg) -Color 'Red'
        }
        Write-Detail ('command: ' + $Command + ' | at ' + $at)
        return $false
    }
}

function Stop-FailedPartway {
    param([string]$FailedStep)
    $script:Outcome = 'FAILED_PARTWAY'
    Write-Host ''
    Write-Host ('STOPPED: step "' + $FailedStep + '" failed. Nothing has been deleted.') -ForegroundColor Red
    Write-Host 'What may exist now (check in Hyper-V Manager and File Explorer; remove by hand only if you want to start over):' -ForegroundColor Red
    Write-Host ('  - VM named: ' + $Name + '  (only if the "Create VM" step above says ACTION DONE)') -ForegroundColor Red
    Write-Host ('  - Folder: ' + [System.IO.Path]::Combine($script:VhdRootFull, $Name)) -ForegroundColor Red
    Write-Host ('  - Disk file: ' + $script:VhdPath + '  (only if the "Create VHDX" step above says ACTION DONE)') -ForegroundColor Red
    Write-Host 'The VM was not started. To be extra safe, if the VM exists run: Set-VM -Name "<name>" -AutomaticStartAction Nothing' -ForegroundColor Red
    $null = Save-CreateRecord -Outcome $script:Outcome
    exit 3
}

function Get-GuestServiceInterface {
    # Name lookup first (English), then the integration-service id suffix (works on other languages).
    param([string]$VmName)
    $services = @(Get-VMIntegrationService -VMName $VmName)
    foreach ($svc in $services) {
        if ($svc.Name -eq 'Guest Service Interface') { return $svc }
    }
    foreach ($svc in $services) {
        $id = [string](Get-PropValue -Object $svc -Name 'Id')
        if ($id -match '6C09BB55-D683-4DA0-8931-C9BF705F6480') { return $svc }
    }
    return $null
}

function New-WorkbenchVm {
    $startupBytes = [long]$MemoryStartupGB * 1GB
    $maxBytes = [long]$MemoryMaxGB * 1GB
    $sizeBytes = [long]$VhdSizeGB * 1GB

    $ok = Invoke-Step -Title 'Create VHDX folder' -Command ('New-Item -ItemType Directory -Force "' + $script:VhdDir + '"') -Action {
        New-Item -ItemType Directory -Path $script:VhdDir -Force | Out-Null
    }
    if (-not $ok) { Stop-FailedPartway 'Create VHDX folder' }

    $ok = Invoke-Step -Title 'Create VHDX (dynamic)' -Command ('New-VHD -Path "' + $script:VhdPath + '" -SizeBytes ' + $sizeBytes + ' -Dynamic') -Action {
        New-VHD -Path $script:VhdPath -SizeBytes $sizeBytes -Dynamic | Out-Null
    }
    if (-not $ok) { Stop-FailedPartway 'Create VHDX (dynamic)' }

    $ok = Invoke-Step -Title 'Create VM (Generation 2)' -Command ('New-VM -Name "' + $Name + '" -Generation 2 -MemoryStartupBytes ' + $startupBytes + ' -VHDPath <vhdx> -SwitchName "' + $SwitchName + '" -Path "' + $script:VhdRootFull + '"') -Action {
        New-VM -Name $Name -Generation 2 -MemoryStartupBytes $startupBytes -VHDPath $script:VhdPath -SwitchName $SwitchName -Path $script:VhdRootFull | Out-Null
    }
    if (-not $ok) { Stop-FailedPartway 'Create VM (Generation 2)' }

    # Safety setting first: the VM must never start by itself when the host boots.
    $ok = Invoke-Step -Title 'Never auto-start; graceful stop on host shutdown' -Command 'Set-VM -AutomaticStartAction Nothing -AutomaticStopAction ShutDown' -Action {
        Set-VM -Name $Name -AutomaticStartAction Nothing -AutomaticStopAction ShutDown
    }
    if (-not $ok) { Stop-FailedPartway 'Never auto-start; graceful stop on host shutdown' }

    $ok = Invoke-Step -Title 'Dynamic memory' -Command ('Set-VMMemory -DynamicMemoryEnabled $true -StartupBytes ' + $startupBytes + ' -MinimumBytes 2GB -MaximumBytes ' + $maxBytes) -Action {
        Set-VMMemory -VMName $Name -DynamicMemoryEnabled $true -StartupBytes $startupBytes -MinimumBytes 2GB -MaximumBytes $maxBytes
    }
    if (-not $ok) { Stop-FailedPartway 'Dynamic memory' }

    $ok = Invoke-Step -Title 'Processor count' -Command ('Set-VMProcessor -Count ' + $ProcessorCount) -Action {
        Set-VMProcessor -VMName $Name -Count $ProcessorCount
    }
    if (-not $ok) { Stop-FailedPartway 'Processor count' }

    $ok = Invoke-Step -Title 'Standard checkpoints, no automatic checkpoints' -Command 'Set-VM -CheckpointType Standard -AutomaticCheckpointsEnabled $false' -Action {
        Set-VM -Name $Name -CheckpointType Standard -AutomaticCheckpointsEnabled $false
    }
    if (-not $ok) { Stop-FailedPartway 'Standard checkpoints, no automatic checkpoints' }

    $ok = Invoke-Step -Title 'Secure Boot (Windows template)' -Command 'Set-VMFirmware -EnableSecureBoot On -SecureBootTemplate MicrosoftWindows' -Action {
        Set-VMFirmware -VMName $Name -EnableSecureBoot On -SecureBootTemplate 'MicrosoftWindows'
    }
    if (-not $ok) { Stop-FailedPartway 'Secure Boot (Windows template)' }

    $ok = Invoke-Step -Title 'Virtual TPM (local key protector)' -Command 'Set-VMKeyProtector -NewLocalKeyProtector; Enable-VMTPM' -Action {
        Set-VMKeyProtector -VMName $Name -NewLocalKeyProtector
        Enable-VMTPM -VMName $Name
    }
    if (-not $ok) { Stop-FailedPartway 'Virtual TPM (local key protector)' }

    $null = Invoke-Step -Optional -Title 'Guest Service Interface (host-to-guest file copy)' -Command 'Enable-VMIntegrationService -Name "Guest Service Interface"' -Action {
        $gsi = Get-GuestServiceInterface -VmName $Name
        if ($null -eq $gsi) { throw 'integration service not found on this VM' }
        Enable-VMIntegrationService -VMName $Name -Name $gsi.Name
    }

    $null = Invoke-Step -Optional -Title 'Video resolution 1920x1080' -Command 'Set-VMVideo -ResolutionType Single -HorizontalResolution 1920 -VerticalResolution 1080' -Action {
        Set-VMVideo -VMName $Name -ResolutionType Single -HorizontalResolution 1920 -VerticalResolution 1080
    }

    if ($script:IsoFull) {
        $dvdOk = Invoke-Step -Optional -Title 'Attach Windows ISO as DVD' -Command ('Add-VMDvdDrive -Path "' + $script:IsoFull + '"') -Action {
            Add-VMDvdDrive -VMName $Name -Path $script:IsoFull
        }
        if ($dvdOk) {
            $null = Invoke-Step -Optional -Title 'DVD first in boot order' -Command 'Set-VMFirmware -FirstBootDevice <DVD drive>' -Action {
                $dvd = @(Get-VMDvdDrive -VMName $Name)[0]
                Set-VMFirmware -VMName $Name -FirstBootDevice $dvd
            }
        }
    }

    $null = Invoke-Step -Optional -Title 'Enhanced Session Mode on the host' -Command 'Set-VMHost -EnableEnhancedSessionMode $true' -Action {
        Set-VMHost -EnableEnhancedSessionMode $true
    }
}

# ---------------------------------------------------------------------------
# Read-back: what Hyper-V reports (not what was requested)
# ---------------------------------------------------------------------------
function Get-BootOrderText {
    param($Firmware)
    $parts = @()
    foreach ($entry in @($Firmware.BootOrder)) {
        $device = Get-PropValue -Object $entry -Name 'Device'
        $devName = [string](Get-PropValue -Object $device -Name 'Name')
        $parts += ([string](Get-PropValue -Object $entry -Name 'BootType') + ':' + $devName)
    }
    return ($parts -join ', ')
}

function Show-ReadBack {
    Write-Host ''
    Write-Host 'WHAT HYPER-V REPORTS NOW (read back from the VM, not what was requested):' -ForegroundColor White

    $null = Invoke-Measure -IsReport -What 'VM state' -Method 'Get-VM (State)' -Requested 'Off' -Probe {
        $vm = Get-VM -Name $Name
        New-MeasureResult -Value ([string]$vm.State) -Display ([string]$vm.State)
    }
    $null = Invoke-Measure -IsReport -What 'Generation' -Method 'Get-VM (Generation)' -Requested '2' -Probe {
        $vm = Get-VM -Name $Name
        New-MeasureResult -Value ([string]$vm.Generation) -Display ([string]$vm.Generation)
    }
    $null = Invoke-Measure -IsReport -What 'AutomaticStartAction (must be Nothing)' -Method 'Get-VM (AutomaticStartAction)' -Requested 'Nothing' -Probe {
        $vm = Get-VM -Name $Name
        New-MeasureResult -Value ([string]$vm.AutomaticStartAction) -Display ([string]$vm.AutomaticStartAction)
    }
    $null = Invoke-Measure -IsReport -What 'AutomaticStopAction' -Method 'Get-VM (AutomaticStopAction)' -Requested 'ShutDown' -Probe {
        $vm = Get-VM -Name $Name
        New-MeasureResult -Value ([string]$vm.AutomaticStopAction) -Display ([string]$vm.AutomaticStopAction)
    }
    $null = Invoke-Measure -IsReport -What 'CheckpointType' -Method 'Get-VM (CheckpointType)' -Requested 'Standard' -Probe {
        $vm = Get-VM -Name $Name
        New-MeasureResult -Value ([string]$vm.CheckpointType) -Display ([string]$vm.CheckpointType)
    }
    $null = Invoke-Measure -IsReport -What 'AutomaticCheckpointsEnabled' -Method 'Get-VM (AutomaticCheckpointsEnabled)' -Requested 'False' -Probe {
        $vm = Get-VM -Name $Name
        $v = Get-PropValue -Object $vm -Name 'AutomaticCheckpointsEnabled'
        if ($null -eq $v) { throw 'property not reported by this Hyper-V version' }
        New-MeasureResult -Value ([string]$v) -Display ([string]$v)
    }
    $null = Invoke-Measure -IsReport -What 'Dynamic memory enabled' -Method 'Get-VMMemory (DynamicMemoryEnabled)' -Requested 'True' -Probe {
        $m = Get-VMMemory -VMName $Name
        $dyn = Get-PropValue -Object $m -Name 'DynamicMemoryEnabled'
        if ($null -eq $dyn) { $dyn = Get-PropValue -Object $m -Name 'DynamicEnabled' }
        if ($null -eq $dyn) { throw 'Get-VMMemory did not report a dynamic-memory property' }
        New-MeasureResult -Value ([string]$dyn) -Display ([string]$dyn)
    }
    $null = Invoke-Measure -IsReport -What 'Memory startup (GB)' -Method 'Get-VMMemory (Startup)' -Requested ([string]$MemoryStartupGB) -Probe {
        $m = Get-VMMemory -VMName $Name
        $gb = [math]::Round(([double]$m.Startup) / 1GB, 2)
        New-MeasureResult -Value ([string]$gb) -Display ([string]$gb)
    }
    $null = Invoke-Measure -IsReport -What 'Memory minimum (GB)' -Method 'Get-VMMemory (Minimum)' -Requested '2' -Probe {
        $m = Get-VMMemory -VMName $Name
        $gb = [math]::Round(([double]$m.Minimum) / 1GB, 2)
        New-MeasureResult -Value ([string]$gb) -Display ([string]$gb)
    }
    $null = Invoke-Measure -IsReport -What 'Memory maximum (GB)' -Method 'Get-VMMemory (Maximum)' -Requested ([string]$MemoryMaxGB) -Probe {
        $m = Get-VMMemory -VMName $Name
        $gb = [math]::Round(([double]$m.Maximum) / 1GB, 2)
        New-MeasureResult -Value ([string]$gb) -Display ([string]$gb)
    }
    $null = Invoke-Measure -IsReport -What 'Virtual processors' -Method 'Get-VMProcessor (Count)' -Requested ([string]$ProcessorCount) -Probe {
        $p = Get-VMProcessor -VMName $Name
        New-MeasureResult -Value ([string]$p.Count) -Display ([string]$p.Count)
    }
    $null = Invoke-Measure -IsReport -What 'Secure Boot' -Method 'Get-VMFirmware (SecureBoot)' -Requested 'On' -Probe {
        $f = Get-VMFirmware -VMName $Name
        New-MeasureResult -Value ([string]$f.SecureBoot) -Display ([string]$f.SecureBoot)
    }
    $null = Invoke-Measure -IsReport -What 'Secure Boot template' -Method 'Get-VMFirmware (SecureBootTemplate)' -Requested 'MicrosoftWindows' -Probe {
        $f = Get-VMFirmware -VMName $Name
        New-MeasureResult -Value ([string]$f.SecureBootTemplate) -Display ([string]$f.SecureBootTemplate)
    }
    $null = Invoke-Measure -IsReport -What 'Boot order' -Method 'Get-VMFirmware (BootOrder)' -Probe {
        $f = Get-VMFirmware -VMName $Name
        $text = Get-BootOrderText -Firmware $f
        New-MeasureResult -Value $text -Display $text
    }
    $null = Invoke-Measure -IsReport -What 'Virtual TPM enabled' -Method 'Get-VMSecurity (TpmEnabled)' -Requested 'True' -Probe {
        $s = Get-VMSecurity -VMName $Name
        New-MeasureResult -Value ([string]$s.TpmEnabled) -Display ([string]$s.TpmEnabled)
    }
    $null = Invoke-Measure -IsReport -What 'Attached hard disk' -Method 'Get-VMHardDiskDrive (Path)' -Requested $script:VhdPath -Probe {
        $d = @(Get-VMHardDiskDrive -VMName $Name)
        if ($d.Count -eq 0) { throw 'no hard disk drive attached' }
        New-MeasureResult -Value ([string]$d[0].Path) -Display ([string]$d[0].Path)
    }
    $null = Invoke-Measure -IsReport -What 'Disk type and sizes' -Method 'Get-VHD -Path (VhdType, Size, FileSize)' -Probe {
        $v = Get-VHD -Path $script:VhdPath
        $text = "{0}, max {1} GB, file on disk now {2} GB" -f $v.VhdType, (Format-GB -Gb ([double]$v.Size / 1GB)), (Format-GB -Gb ([double]$v.FileSize / 1GB))
        New-MeasureResult -Value ([string]$v.VhdType) -Display $text
    }
    $null = Invoke-Measure -IsReport -What 'Connected switch' -Method 'Get-VMNetworkAdapter (SwitchName)' -Requested $SwitchName -Probe {
        $n = @(Get-VMNetworkAdapter -VMName $Name)
        if ($n.Count -eq 0) { throw 'no network adapter' }
        New-MeasureResult -Value ([string]$n[0].SwitchName) -Display ([string]$n[0].SwitchName)
    }
    $null = Invoke-Measure -IsReport -What 'Guest Service Interface enabled' -Method 'Get-VMIntegrationService (Enabled)' -Requested 'True' -Probe {
        $g = Get-GuestServiceInterface -VmName $Name
        if ($null -eq $g) { throw 'integration service not found' }
        New-MeasureResult -Value ([string]$g.Enabled) -Display ([string]$g.Enabled)
    }
    $null = Invoke-Measure -IsReport -What 'Video resolution' -Method 'Get-VMVideo' -Probe {
        $v = Get-VMVideo -VMName $Name
        $text = "{0} {1}x{2}" -f $v.ResolutionType, $v.HorizontalResolution, $v.VerticalResolution
        New-MeasureResult -Value $text -Display $text
    }
    if ($script:IsoFull) {
        $null = Invoke-Measure -IsReport -What 'DVD drive media' -Method 'Get-VMDvdDrive (Path)' -Requested $script:IsoFull -Probe {
            $d = @(Get-VMDvdDrive -VMName $Name)
            if ($d.Count -eq 0) { throw 'no DVD drive' }
            New-MeasureResult -Value ([string]$d[0].Path) -Display ([string]$d[0].Path)
        }
    }
    $null = Invoke-Measure -IsReport -What 'Enhanced Session Mode (host setting)' -Method 'Get-VMHost (EnableEnhancedSessionMode)' -Requested 'True' -Probe {
        $h = Get-VMHost
        New-MeasureResult -Value ([string]$h.EnableEnhancedSessionMode) -Display ([string]$h.EnableEnhancedSessionMode)
    }
}

# ---------------------------------------------------------------------------
# Next steps for the human
# ---------------------------------------------------------------------------
function Get-NextSteps {
    $steps = New-Object System.Collections.Generic.List[string]
    if (-not $script:IsoFull) {
        $steps.Add('No ISO was attached. In Hyper-V Manager right-click the VM, choose Settings. You will see the hardware list.')
        $steps.Add('Click SCSI Controller, click DVD Drive, click Add. Choose Image file, click Browse, pick your Windows ISO, click OK.')
        $steps.Add('In Settings click Firmware and move DVD Drive to the top of the boot order with the Move Up button, then click OK.')
    }
    $steps.Add('Press the Windows key, type Hyper-V Manager and press Enter. You will see a window with your computer name on the left.')
    $steps.Add('Click your computer name on the left. You will see the virtual machine "' + $Name + '" in the middle with State: Off.')
    $steps.Add('Double-click "' + $Name + '". You will see a black Virtual Machine Connection window with a Start button.')
    $steps.Add('Click Start (the green power button in that window). You will see the Hyper-V firmware screen appear.')
    $steps.Add('Click inside the window. When it says "Press any key to boot from CD or DVD" press the Space bar within a few seconds. If you miss it use Action > Reset and try again.')
    $steps.Add('You will see "Windows Setup". Choose language, time and keyboard, click Next, click Install now.')
    $steps.Add('If asked for a product key click "I don''t have a product key" (an evaluation ISO does not need one). Pick the edition if asked and accept the licence terms.')
    $steps.Add('Choose "Custom: Install Windows only (advanced)". You will see one disk of about ' + $VhdSizeGB + ' GB, Unallocated Space.')
    $steps.Add('Select that disk and click Next. You will see copying and several automatic restarts (20 to 40 minutes).')
    $steps.Add('After the first restart do NOT press a key at "Press any key to boot from CD or DVD". Let it boot from the disk.')
    $steps.Add('Answer the first-run questions (region, keyboard). Create a local user used only for the workbench. Do not use your personal account or password.')
    $steps.Add('When the Windows desktop appears, shut Windows down from inside the VM: Start, Power, Shut down.')
    $steps.Add('On the host, in an elevated PowerShell, run: .\Workbench.ps1 -Action Checkpoint -Label "clean install". This saves a clean starting point.')
    $steps.Add('Later, start the VM with: .\Workbench.ps1 -Action Start and look at it with: .\Workbench.ps1 -Action Status')
    return $steps.ToArray()
}

function Write-NextSteps {
    Write-Host ''
    Write-Host ('THIS SCRIPT DID NOT START THE VM. Hyper-V reports its state as shown above (expected: Off).') -ForegroundColor White
    Write-Host 'The VM will not start by itself when the host boots (AutomaticStartAction = Nothing, see read-back above).' -ForegroundColor White
    Write-Host ''
    Write-Host 'NEXT STEPS (you do these by hand, one action per line):' -ForegroundColor White
    $n = 0
    foreach ($line in (Get-NextSteps)) {
        $n++
        Write-Host ('{0,3}. {1}' -f $n, $line)
    }
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
function Invoke-Checks {
    Write-Host ('AVNI Agent Workbench - create VM - ' + (Get-Stamp)) -ForegroundColor White
    Write-Host 'Checks first (read-only). Every line shows what was measured, the value, the method and the time.'
    Write-Host ''
    Test-Prerequisites
    Test-StorageTarget
    Test-OtherInputs
    Write-Host ''
}

function Invoke-Creation {
    Write-Host 'CREATING (each line only says the cmdlet returned without error; the read-back below is the evidence):' -ForegroundColor White
    New-WorkbenchVm
    Show-ReadBack

    $auto = @($script:Reported | Where-Object { $_.What -like 'AutomaticStartAction*' })
    if ($auto.Count -eq 0 -or $auto[0].Status -ne 'MEASURED' -or $auto[0].Value -ne 'Nothing') {
        Add-Warning 'AutomaticStartAction is NOT confirmed as Nothing. Run: Set-VM -Name "<name>" -AutomaticStartAction Nothing, then check Get-VM again.'
    }
    $differs = @($script:Reported | Where-Object { $_.MatchesRequest -eq $false })
    if ($differs.Count -gt 0) {
        Add-Warning ($differs.Count.ToString() + ' read-back value(s) differ from the request (red lines above). Review them before first start.')
    }
    $script:Outcome = 'CREATED_NOT_STARTED'
    Write-NextSteps
}

try {
    Invoke-Checks
}
catch {
    Stop-Refused ('Unexpected error while checking, so nothing was created: ' + (Limit-Text -Text $_.Exception.Message -Max 300))
}

$plan = "Create Generation 2 VM '$Name' ($MemoryStartupGB-$MemoryMaxGB GB dynamic memory, $ProcessorCount CPUs, dynamic disk up to $VhdSizeGB GB) at $($script:VhdPath)"
if (-not $PSCmdlet.ShouldProcess($Name, $plan)) {
    Write-Host 'WhatIf/Confirm: no changes were made. All checks above passed.' -ForegroundColor Yellow
    exit 0
}

try {
    Invoke-Creation
}
catch {
    Write-Host ('UNEXPECTED ERROR after creation started: ' + (Limit-Text -Text $_.Exception.Message -Max 300)) -ForegroundColor Red
    Write-Host ('Something may exist already (VM "' + $Name + '", folder under ' + $script:VhdRootFull + '). Nothing was deleted and the VM was not started.') -ForegroundColor Red
    $null = Save-CreateRecord -Outcome 'FAILED_PARTWAY'
    exit 3
}
$recordPath = Save-CreateRecord -Outcome $script:Outcome
if ($null -eq $recordPath) { exit 2 }
exit 0
