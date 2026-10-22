<#
.SYNOPSIS
    AVNI Agent Workbench - install the Microsoft 365 apps (Excel, Word, PowerPoint, Outlook) INSIDE the guest.

.DESCRIPTION
    Why: Ravi 21/22-10-2026 - Excel teaching runs inside the Workbench on a Microsoft 365 subscription he buys; the
    laptop's own Office Home 2024 is a one-PC licence and is not stretched into the VM.
    What: Microsoft's Office Deployment Tool (setup.exe from officecdn.microsoft.com) with a silent configuration.
    Product O365HomePremRetail = the Microsoft 365 Personal / Family apps. If Ravi buys a Business plan instead, run
    again with -Product O365BusinessRetail.
    After the install the apps open unlicensed until Ravi signs in INSIDE the Workbench: open Excel > Sign in > the
    Microsoft account he buys the subscription on. Nothing is bought or signed in by this script.
#>
param(
    [string]$Product = 'O365HomePremRetail',
    [string]$Language = 'en-us'
)
$ErrorActionPreference = 'Stop'
$dir = 'C:\AVNI\m365'
New-Item -ItemType Directory -Force $dir | Out-Null
$excel = 'C:\Program Files\Microsoft Office\root\Office16\EXCEL.EXE'
if (Test-Path $excel) { "already installed: $excel"; exit 0 }
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$setup = Join-Path $dir 'setup.exe'
Invoke-WebRequest -Uri 'https://officecdn.microsoft.com/pr/wsus/setup.exe' -OutFile $setup -UseBasicParsing
"setup.exe downloaded: $([math]::Round((Get-Item $setup).Length/1MB,1)) MB, signer: " + (Get-AuthenticodeSignature $setup).SignerCertificate.Subject
if ((Get-AuthenticodeSignature $setup).Status -ne 'Valid' -or (Get-AuthenticodeSignature $setup).SignerCertificate.Subject -notmatch 'Microsoft Corporation') { throw 'setup.exe is not validly signed by Microsoft - stopping' }
@"
<Configuration>
  <Add OfficeClientEdition="64" Channel="Current">
    <Product ID="$Product">
      <Language ID="$Language" />
      <ExcludeApp ID="Groove" />
      <ExcludeApp ID="Lync" />
      <ExcludeApp ID="Teams" />
    </Product>
  </Add>
  <Display Level="None" AcceptEULA="TRUE" />
  <Property Name="AUTOACTIVATE" Value="0" />
  <Updates Enabled="TRUE" />
</Configuration>
"@ | Set-Content -Path (Join-Path $dir 'config.xml') -Encoding utf8
$t0 = Get-Date
$p = Start-Process -FilePath $setup -ArgumentList '/configure', (Join-Path $dir 'config.xml') -Wait -PassThru
"setup /configure exit $($p.ExitCode) after $([int]((Get-Date) - $t0).TotalMinutes) min"
"Excel present: " + (Test-Path $excel)
if (Test-Path $excel) { exit 0 } else { exit 1 }
