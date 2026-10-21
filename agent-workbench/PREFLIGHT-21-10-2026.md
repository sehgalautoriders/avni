# Workbench 01 - Preflight on the PC, 21-10-2026 17:49-17:52

Step A of `RESUME-LOCAL-SESSION.md`, run by the local Claude Code chat on the PC (Ravi pasted the resume prompt; handshake job HS-20261019-090459 carries the pickup note). Raw reports stay on the PC (`%LOCALAPPDATA%\Avni\reports\workbench-preflight-21-10-2026_1749.*` plain, `_1750.*` elevated); this repository is public, so only this summary is committed.

Script: `host/Preflight-Workbench.ps1`, Windows PowerShell 5.1. Plain run 17 s, elevated run 41 s (UAC pressed by Ravi at the PC). Verdict both runs: **READY WITH CONDITIONS**.

| Item | Measured value (elevated run) | Method |
|---|---|---|
| Edition | Windows 11 Pro 25H2, build 26200 - supports Hyper-V | Win32_OperatingSystem + registry |
| Hypervisor running | True (VBS) | Win32_ComputerSystem |
| Hyper-V optional feature | **Disabled** at 17:50 (measured elevated) | Get-WindowsOptionalFeature |
| Hyper-V module / vmms | not present at 17:50 | Get-Module, Get-Service |
| RAM | 63.3 GB total, 29.8 GB free -> 8 GB VM recommended | Win32_OperatingSystem |
| Fixed drives | C: 475.9 / 57.7 GB free (system, excluded); E: 931.5 / 845.9 GB free (USB SanDisk SSD); G:, I:, M:, R: are mappings of C: | Win32_LogicalDisk |
| 2 TB HDD | **not connected** | Get-Disk |
| GPU | AMD Radeon (integrated); no GPU partitioning on Pro | Win32_VideoController |

## Done after the preflight (same hour)

- 17:52, elevated, as Ravi answered on his phone on 20-10-2026 ("I will plug in the 2 TB disk and allow Hyper-V"): `Enable-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V-All -All -NoRestart`. Result: all seven Hyper-V features **Enabled**, `RestartNeeded = True`. No restart was forced; the feature completes at the next power-off / restart. Log: `%LOCALAPPDATA%\Avni\reports\workbench-hyperv-enable-21-10-2026.log`.

## What still stops step B onwards

1. **The 2 TB HDD is not connected** (physical act, owner). Decision 2 excludes C: and the AVNI OS SSD (E:), so no VM disk is allocated until it is plugged in and measured (item 30).
2. **Restart pending** for Hyper-V. Until then `New-Workbench.ps1` cannot run (no Hyper-V module, no vmms).

Nothing was created or allocated on any disk for the VM. The evaluation ISO (decision 1) is fetched to `E:\AVNI-Workbench\iso\` through the internet wall; E: is the detachable overflow drive and the ISO can be re-downloaded.
