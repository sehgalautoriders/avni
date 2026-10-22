# Workbench 01 - Preflight on the PC, 19-10-2026 09:06

Step A of `RESUME-LOCAL-SESSION.md`. Run by the PC headless session (handshake job HS-20261019-090459), **not elevated**. Raw report kept on the PC only (`%LOCALAPPDATA%\Avni\reports\workbench-preflight-19-10-2026_0906.{json,txt}`); this repository is public, so only this summary is committed.

Script: `host/Preflight-Workbench.ps1` 1.0.0, Windows PowerShell 5.1, plain run. Verdict: **READY WITH CONDITIONS**, no blockers from the script itself.

| Item | Measured value | Method |
|---|---|---|
| Edition | Windows 11 Pro 25H2, build 26200.9711 - supports Hyper-V | Win32_OperatingSystem + registry |
| CPU | Ryzen 7 5825U, 8 cores / 16 threads, firmware virtualisation on | Win32_Processor |
| Hypervisor running | True (VBS status 2 = running); Hyper-V itself NOT installed | Win32_ComputerSystem, Win32_DeviceGuard |
| Hyper-V module / vmms / vmcompute | not present | Get-Module, Get-Service |
| Hyper-V feature, VMs, switches | NOT MEASURED (needs admin) | - |
| RAM | 31.3 GB total, 10.2 GB free -> 6 GB VM recommended | Win32_OperatingSystem |
| Other hypervisors | VirtualBox, VMware, Docker: none; WSL not installed | registry, wsl.exe |
| GPU | AMD Radeon (integrated); no GPU-P on Pro, guest renders in software | Win32_VideoController |

## Drives (decision 2)

| Letter | Physical disk | Bus | Size / free | Health | Eligible? |
|---|---|---|---|---|---|
| C: | Samsung MZVL8512 NVMe SSD | NVMe | 475.9 / 114.3 GB | Healthy | **No** - system drive, decision 2 |
| E: | SanDisk Extreme 55DD SSD, label "SSD" | USB | 931.5 / 886.2 GB | Healthy | **No** - taken to be the SSD kept for AVNI OS (formatted clean 18-10-2026; earlier notes: "AVNI OS start on a clean SSD"), decision 2. Owner to confirm |
| 2 TB HDD (WD My Passport) | not connected | - | - | - | candidate, **absent** |

The script's own rule recommends E: (most free space). That rule does not know decision 2, so the recommendation is **overruled**: no VM disk is allocated.

## What stops Step B onwards (stated, not guessed)

1. **No eligible drive is connected.** The 2 TB HDD must be plugged in before item 30 (health + timed 1 GB write/read) can be measured. Physical act - owner.
2. **Hyper-V is not installed.** Enabling `Microsoft-Hyper-V-All` needs an elevated prompt (Windows permission) and a restart. Owner.
3. Elevated preflight re-run waits on point 2's permission.

Nothing was created, enabled, downloaded or allocated. C: and E: untouched.
