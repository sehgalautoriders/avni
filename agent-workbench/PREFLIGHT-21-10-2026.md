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

## Step B - drive decision and measurement (21-10-2026 18:2x)

Ravi, 21-10-2026: "There will be no HDD 2 TB, 1 TB SSD is attached" and "All programs stay on C only. Files from top priority to 1 which the program needs immediately stay on C. Others move to E."

- **Programs on C:** the host scripts, the guest hands service, the host driver and the run folders (`AVNI_WORKBENCH_DATA` = `%LOCALAPPDATA%\Avni\workbench\runs`, set as a User variable).
- **VM data on E:** `E:\AVNI-Workbench\AVNI-VMs\` for the VHDX and checkpoints, `E:\AVNI-Workbench\iso\` for the evaluation ISO. E: is the detachable overflow drive; the Workbench is OFF by default and can be rebuilt from the ISO. C: has 58 GB free, below the 100 GB floor, so it cannot hold a VHDX.

| E: measurement | Value | Method |
|---|---|---|
| Disk | SanDisk Extreme 55DD, SSD, USB, 931.5 GB, 839 GB free | Get-PhysicalDisk, Get-Volume |
| Health | HealthStatus Healthy, OperationalStatus OK (reliability counters not exposed over USB) | Get-PhysicalDisk |
| 1 GB write, WriteThrough | 68.5 s = 15 MB/s | FileStream, 16 x 64 MB, Flush(true) |
| 1 GB write, buffered + final flush | 34.3 s = 30 MB/s | FileStream, 16 x 64 MB, Flush(true) |
| 1 GB read | 0.6 s - served from the cache, not a disk number | FileStream |

30 MB/s is USB-2 class for a drive that does hundreds of MB/s on USB 3. The USB link is being checked; the VM works at this speed, Windows setup is just slower.

## Steps C and D - VM created, isolation applied, adapter connected (21-10-2026 18:46-18:59)

- **Restart at 18:36** (Ravi) completed the Hyper-V install: vmms and vmcompute running, Hyper-V module present.
- **Step C, 18:46, elevated:** `New-Workbench.ps1 -VhdRoot E:\AVNI-Workbench\AVNI-VMs -MemoryStartupGB 6 -MemoryMaxGB 8 -ProcessorCount 4`. Read back by the script: Gen 2, dynamic VHDX 200 GB, dynamic memory 6/2/8 GB, 4 vCPU, Secure Boot (MicrosoftWindows), vTPM, Guest Service Interface on, adapter **disconnected**, no automatic start, State Off. No ISO attached yet.
- **Step D, first run 18:49:** Apply wrote 21 rules and read them back; **Connect FAILED** with Hyper-V error 0x800700B7 ("Cannot create a file when that file already exists") while binding the adapter to the Default Switch.
- **Cause, measured 18:5x (elevated test):** with the adapter on a live switch port, a second Extended ACL rule with the same Direction and Weight is rejected with exactly that error; a rule with a different weight is accepted; IPv6 rules are accepted. The plan had five Outbound and five Inbound deny rules at weight 50.
- **Fix:** `Set-WorkbenchNetworkIsolation.ps1` 1.1.0 gives every rule its own weight (Outbound deny 50-56, Inbound deny 40-46, host addresses 60 and up, ExtraAllow 80 and up, gateway 90/98/99/100, Inbound hands 101) and refuses a plan that has a duplicate Direction+Weight before touching the switch.
- **Step D, run 2, 18:58:** Apply 21 rules, read back 21; Show: inventory MATCHES the adapter, baseline deny rules all present; **Connect: SwitchName = Default Switch, Connected = True**, VM still Off.
- Behaviour proof (LAN unreachable, internet reachable, DNS works) is step F and needs the guest running.

## Step E prepared - unattended Windows setup

`autounattend.xml` (UEFI partitions, image index 1, local account, OOBE and online-account screens hidden, India Standard Time, auto-logon) built into `E:\AVNI-Workbench\iso\autounattend.iso` with Windows' own IMAPI2 (no ADK needed). The guest account record lives only on the PC. Setup therefore needs nobody clicking inside vmconnect, which also keeps Ravi's keyboard his own.

## Step E - Windows installed unattended (21-10-2026 20:16-23:43)

- ISO complete 20:11 (8,225,329,152 bytes; mounted, `sources\install.wim` and `setup.exe` present). A first copy written by two download processes at once was found corrupt and removed; the clean copy was downloaded again from zero by one process.
- 20:12 both ISOs attached as DVD drives, Windows ISO first in the boot order; 20:16 `Start-VM`; Enter sent through WMI `Msvm_Keyboard.TypeKey` for the "Press any key" prompt (33 accepted). Nobody clicked in vmconnect.
- The host's power-off/boot at 22:36 switched the VM Off mid-way (Hyper-V keeps no state on a power-off). A minder restarted it 23:32; "Just a moment" (first run) until 23:42, then PowerShell Direct answered: `AVNI-WB01\agent`, OOBEInProgress 0, explorer running. VHDX 16.6 GB.
- 23:43:56 checkpoint `21-10-2026 2342 clean install` created and read back (`Workbench.ps1 -Action Checkpoint`).
- Guest screen thumbnails without vmconnect: WMI `GetVirtualSystemThumbnailImage` (RGB565 640x360) saved under `%LOCALAPPDATA%\Avni\workbench\runs\`.

## Step F - first run NOT MEASURED, cause found (23:46-23:52)

`Test-WorkbenchIsolation.ps1` from inside the guest: every LAN and internet row failed instantly, DNS failed, verdict NOT MEASURED (positive control failed) and the script correctly refused to run step G. On the host: the 22:36 boot gave the Default Switch a **new gateway 172.26.32.1/20** (the rules were built for 172.17.224.1). The new gateway lies inside the denied 172.16.0.0/12 range, so the guest gets no DHCP lease, no DNS and no NAT. This is the case the cloud session predicted; the cure is `-Action Apply` again for the live gateway, then `ipconfig /renew` in the guest, then the test. The minder now does this at every run, since this PC is powered off every evening.

## Step F - ISOLATION VERIFIED (22-10-2026 00:01)

Heal first (one elevated run, 23:54): `-Action Disconnect` (VM kept running), `-Action Apply` rebuilt the 21 rules for the live gateway 172.26.32.1 (21 old removed, 21 new read back), `-Action Show` inventory MATCHES, `-Action Connect` SwitchName Default Switch, Connected True; then `ipconfig /renew` in the guest: 172.26.35.77/20, gateway and DNS 172.26.32.1, TCP 443 to 1.1.1.1 True.

`Test-WorkbenchIsolation.ps1` over PowerShell Direct, 23:57-00:01, exit 0:

| Group | Target | Check | Measured | Result |
|---|---|---|---|---|
| Host ACL | VM adapter | 10 deny rules | all present | PASS |
| LAN | 10.199.27.85, 192.168.1.10, 192.168.1.254 | TCP 445, TCP 3389 | timed out 21-24 s each | PASS |
| LAN | same three | ICMP | no reply (weak evidence) | PASS |
| Internet | 1.1.1.1, 8.8.8.8 | TCP 443 | connected 0-0.1 s | PASS |
| Internet | www.microsoft.com | DNS A | 23.37.86.48 | PASS |
| Info | 172.26.32.1 (host) | TCP 445, 3389 | no answer | INFO |

Report: `%LOCALAPPDATA%\Avni\reports\workbench-isolation-test-21-10-2026_2357.json`. Limit stated by the test: "no answer" cannot tell the switch ACL from a target's own firewall; the ACL itself is proven by the read-back.

**Standing rule from this:** the PC is powered off every evening and the Default Switch gateway changes on boot, so before every use run Disconnect, Apply, Connect (the minder does it). A fixed-gateway internal NAT switch would remove the need and is the proper follow-up.
