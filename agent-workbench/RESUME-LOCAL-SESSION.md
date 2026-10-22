# AVNI Agent Workbench 01 - resume in a LOCAL Claude Code session on the PC

Decision date 18-10-2026 (Architect). Source of truth for code: GitHub `sehgalautoriders/avni`, PR #1, branch `ccr-36510265-9lc0mz`. Use the live repository record for commit count and head, never a number copied from an older chat.

Paste section 1 as the first message of the local session.

## 1. Resume prompt (paste as is)

```
First line: C:\Users\sehga\Desktop\copilotremap.ahk is part of my keyboard, not an app. Never end, freeze or clean up AutoHotkey.

Continue AVNI Agent Workbench 01 from GitHub sehgalautoriders/avni PR #1 (branch ccr-36510265-9lc0mz, take the live head). Do not restart the design, do not overwrite other agents' work, do not merge the PR. Read agent-workbench/README.md and agent-workbench/RESUME-LOCAL-SESSION.md in full first, then work the order below. Report each milestone as DESIGNED / CODED / TESTED / INSTALLED / VERIFIED with measurement method and time. If preflight shows a genuine blocker, stop that step, keep the evidence, report.

Decisions already taken, do not ask again:
1. Guest Windows = official 90-day Windows 11 Enterprise evaluation ISO (Microsoft Evaluation Center).
2. VM disk = chosen only after preflight, from measured suitable fixed drives. C: untouched. The SSD reserved for AVNI OS is excluded. The 2 TB HDD is the candidate if healthy, roomy and fast enough (measure it).
   RAVI 21-10-2026 (overrides the HDD part): "There will be no HDD 2 TB, 1 TB SSD is attached." and "All programs stay on C only. Files from top priority to 1 which the program needs immediately stay on C. Others move to E." -> VM data (VHDX, checkpoints, ISO) on E:\AVNI-Workbench\; programs and run folders on C:. Measured E: 21-10: Healthy, 839 GB free, 1 GB write 30 MB/s buffered / 15 MB/s write-through.
3. Guest-to-LAN block BEFORE the first network-connected guest boot. The VM is prepared offline. Only host-to-guest control (TCP 7001) and public internet are allowed.
4. One workbench, one agent. No clones.
5. Register in AVNI Sentinel as installed but OFF. Never a permanently running process.

Order of work (each step is measured, nothing green from a proxy):
A. Preflight, read-only: run agent-workbench\host\Preflight-Workbench.ps1 (plain, then elevated). Save the report. Record edition, hypervisor state, RAM free, every fixed drive with type and free space, existing VMs/switches, VirtualBox/WSL/Docker presence, GPU. Then measure the 2 TB HDD: Get-PhysicalDisk HealthStatus, free space, and a timed 1 GB write/read. Put the numbers into TRACKER.xlsx items 2 and 30.
B. Drive decision from the numbers (decision 2). Set AVNI_WORKBENCH_DATA to a folder on that drive for run folders.
C. Create the VM offline: elevated, New-Workbench.ps1 -VhdRoot <drive>\AVNI-VMs -IsoPath <evaluation ISO>. The adapter is created disconnected. Do not pass -EnableEnhancedSession unless I say so.
D. Isolation before connection: Set-WorkbenchNetworkIsolation.ps1 -Action Apply, then -Action Show, then -Action Connect (it refuses if the deny rules are missing).
E. Windows setup by hand in vmconnect (local account, no Microsoft account). Then Workbench.ps1 -Action Checkpoint -Label "clean install".
F. Prove isolation: Test-WorkbenchIsolation.ps1 (PowerShell Direct). Required result: LAN targets unreachable, internet reachable, DNS works. Put the verdict and report path in the tracker. Do not proceed to G until ISOLATION VERIFIED.
G. Inside the guest: winget install Chrome, VS Code, Git, Python, PowerShell 7; then guest\Install-Hands.ps1 (no -EnableShell unless I say so). Copy hands.token to the host once.
H. Host: Workbench.ps1 -Action Ip; then python host\workbench_driver.py --guest <ip> --dry-run-model. Required: 14 actions, 0 unexpected failures, against the REAL guest.
I. First real task: --task "Open Notepad, type today's date, save to Desktop as test.txt". Watch in vmconnect. Record the run folder.
J. Acceptance demos 1 to 5 (README section 5), each with measurement method. Host cursor position and foreground window measured before and after.
K. Sentinel registration as installed-but-OFF, ledger entry, tracker update, mirror the folder into D:\AE Brain, handoff .md on the Desktop.
Read-only viewer (worker card WC-03) remains an acceptance requirement; until it exists, watch in vmconnect and do not click inside it.
```

## 2. Milestone ledger (state on 18-10-2026, from the cloud session)

| Component | DESIGNED | CODED | TESTED (where) | INSTALLED | VERIFIED on PC |
|---|---|---|---|---|---|
| Specification, tracker, cards | yes | yes | n/a | n/a | n/a |
| Preflight-Workbench.ps1 | yes | yes | parse + mocked cmdlets, Linux | no | no |
| New-Workbench.ps1 (adapter disconnected by default) | yes | yes | parse, Linux | no | no |
| Set-WorkbenchNetworkIsolation.ps1 (Extended Port ACLs) | yes | yes | parse, Linux | no | no |
| Test-WorkbenchIsolation.ps1 (PowerShell Direct proof) | yes | yes | parse, Linux | no | no |
| Workbench.ps1 operations | yes | yes | parse + mocked cmdlets, Linux | no | no |
| Guest hands service 0.2.0 | yes | yes | 54 unit tests + dry run, Linux | no | no |
| Install-Hands.ps1 (firewall limited to host) | yes | yes | parse, Linux | no | no |
| Host driver (computer toolset) | yes | yes | dry run vs guest service, Linux | no | no |
| Read-only viewer | card WC-03 | no | no | no | no |
| Sentinel registration | rule given | no | no | no | no |

Nothing is INSTALLED or VERIFIED. The cloud session cannot change that; the local session can.

## 3. Why isolation is host-side

The guest hands service already refuses clients other than the host. That controls who can drive the guest. It does nothing about where the guest can go. Hyper-V NAT on the Default Switch lets the guest reach the owner's LAN. The fix is Extended Port ACLs on the VM's virtual adapter, applied by the host and enforced by the virtual switch. An agent with administrator rights inside the guest cannot remove them. The test proves it from inside the guest over PowerShell Direct, which needs no network.

## 4. Known limits to state in every report

- Token travels over plain HTTP on the host-guest link. Accepted for the pilot.
- vmconnect takes the owner's keyboard when clicked inside. Read-only viewer is an acceptance requirement.
- Checkpoints are rollback, not backup.
- The evaluation ISO expires after 90 days.
