# AVNI Agent Workbench 01

A separate Windows computer, running inside a window on Desktop 1, where an AI agent has its own screen, mouse and keyboard and works like a software engineer. Ravi keeps his own mouse, keyboard and applications. Nothing the agent clicks reaches the host.

Status of this folder on 10-10-2026: specification, tracker and first scripts. **Nothing has been installed or changed on the PC.** Every script here was written from a Linux cloud session and is `CONFIGURED - UNVERIFIED` until it runs on the real machine. The tracker (`TRACKER.xlsx`) is the single list of what is Done, Partial, Pending.

## 1. What Ravi asked for, clause by clause

| Clause (Ravi's words) | Reading | Status |
|---|---|---|
| "a separate workspace inside the desktop where the agent could work and not affect the main program" | A separate operating-system instance, not a second window of the same Windows session. Hyper-V virtual machine. | Specified. Scripts written. Not yet created on the PC. |
| "it can show me the changes immediately in that window" | A live viewer window on Desktop 1 showing the guest screen. | vmconnect.exe gives this today (Stage 1). Own read-only viewer is a worker card (Stage 3). |
| "hands and legs like a proper human being ... operate the mouse and keyboard aggressively in that window" | Real mouse and keyboard events inside the guest via `SendInput`, driven by Claude's computer-use toolset. | Guest service and host driver written. Unverified on Windows. |
| "First we will start with 1 window and then make multiple parallel copies" | One VM now. Clone from a golden image later. | One VM specified. Cloning is Stage 5, not started. |
| "In the universe 1 also you guys do not work with the mouse and keyboard since that will affect the main desktop" | Correct diagnosis. Universes share the host's input queue. This design does not. | Explained in section 3. |
| "Any program required ... so that agents learn there" | Guest has internet (NAT) and can install software with winget. Every run is recorded as screenshots + actions for later learning. | Run folder recording built into the driver. Learning pipeline not built. |

## 2. What is different from a Universe

| Capability | Universe (today) | Workbench |
|---|---|---|
| Separate Windows OS | No, same session | Yes, Hyper-V guest |
| GUI applications | Only what the host shows | Full Windows desktop |
| AI mouse and keyboard | Would hijack Ravi's | Guest-only, through `SendInput` inside the guest |
| Ravi's input unaffected | Not assured | By design: separate input queue, separate desktop |
| Install and test programs | Shared-environment risk | Inside the guest only |
| Save, restart, resume | Depends | Persistent VHDX, checkpoints |
| Record what the agent saw and did | No | Every step in a run folder |

## 3. Architecture

```
Desktop 1 (Ravi's Windows 11 Pro)
 |
 |-- vmconnect.exe window  <-- Ravi WATCHES here (clicking inside sends HIS keyboard; click outside to take it back)
 |
 |-- host/workbench_driver.py  (Python, calls Claude with computer_toolset_20260801)
 |        |  HTTP, token header, TCP 7001 over the Hyper-V Default Switch
 |        v
 |   [ Hyper-V guest: AVNI Agent Workbench 01 ]
 |        guest/avni_hands.py  --> user32 SendInput (mouse, keyboard), ImageGrab (screen)
 |        Chrome, VS Code, Git, Python, PowerShell 7, whatever the agent installs
 |
 |-- host/Workbench.ps1  Status / Start / Shutdown / Checkpoint / Restore / Ip / Connect / Copy
```

Why the input is isolated: Windows keeps one input queue per interactive session. The guest is a different machine with its own session. `SendInput` called inside the guest can only reach the guest's desktop. The viewer on the host only displays pixels; the agent's actions never pass through it.

Why Hyper-V and not Windows Sandbox: Sandbox discards everything on close. The agent must keep its tools and work.

Why not GPU passthrough: not supported on Windows 11 Pro Hyper-V. Guest rendering is software. Fine for IDEs and browsers. Not for 3D.

## 4. Dissent upward (what I believe, what the Boss may not be seeing)

The architect's proposal is right on the main decision: one true, persistent, isolated Windows VM with a guest-resident input service. I agree with it. These are the points I would add before anyone spends an hour on it.

1. **The hard part is not the VM, it is the loop cost and speed.** Every agent step is a screenshot sent to the model. At roughly 1,500 tokens per 1568-px screenshot plus reasoning, a 60-step GUI task is on the order of 150,000 to 300,000 input tokens. On Opus 5.5 that is around 1 to 2 US dollars per task, and 2 to 5 seconds per step. "Aggressive" GUI work is slower and dearer than the same work in a terminal. Recommendation: the agent does GUI where GUI is the point (testing the dashboard buttons, using Visual Studio dialogs) and still uses the guest terminal for bulk edits. The driver's `/shell` endpoint exists for this reason.
2. **Windows licence inside the guest is a real cost line.** A Windows 11 Pro licence on the host does not cover a second Windows instance. Zero-cost pilot path: the 90-day Windows 11 Enterprise evaluation ISO from Microsoft. After 90 days it must be rebuilt or licensed. Owner decision. Tracker item 3.
3. **The viewer can still steal Ravi's keyboard.** vmconnect captures the keyboard the moment he clicks inside it. That is not hijacking, but it will feel like it the first time. Stage 3 builds a read-only viewer that never takes input. Until then the rule is: watch, do not click inside.
4. **RAM and disk are shared even when input is not.** A 6 to 8 GB guest on a 16 GB host leaves the host tight. Preflight measures this before anything is created. No guess is written anywhere; the script prints the numbers.
5. **Hyper-V changes how VirtualBox and WSL behave.** Turning on Hyper-V makes VirtualBox run in its slow mode. If a Universe depends on VirtualBox today, it slows down. WSL 2 already uses the Hyper-V platform, so it is unaffected. Preflight reports what is installed.
6. **The agent inside the guest must have no path to production.** Default Switch is NAT: the guest can reach the internet and the host's IP, not host drives. Rules for Stage 4: no mapped drives, no host credentials in the guest, a guest-only Git identity limited to a review branch, no production database connection strings. Code moves out through Git review, never by copy.
7. **Checkpoints are rollback, not backup.** A checkpoint lives on the same disk as the VM. Disk failure takes both.

What I would do if I owned it: run Preflight tomorrow, make decisions 3 and 4 from the printed numbers, create the VM, install Windows by hand once, install the hands service, and spend the first agent run on the five acceptance demos below. Only then talk about clones.

## 4a. Review round 1 (Architect, 10-10-2026) and what changed

The Architect read PR #1 on GitHub and returned two decisions and five findings. All five are addressed on the branch; two are fully closed in code, three are closed as far as code can close them and the rest is a Stage 4 item.

| # | Finding | What changed | State |
|---|---|---|---|
| F1 Critical | Hands service answered any client on the virtual network | Service refuses every client except localhost and the default gateway (the host on Default Switch) with 403 before the token check. `--allow-client` / `--allow-any-client` widen it only on purpose. Installer firewall rule now limited to the host address. | Done in code, 13 new tests pass on Linux. Verify in the guest. |
| F2 High | Screenshots and run records land on C: | Host driver honours `AVNI_WORKBENCH_DATA` (point it at the approved drive). Guest archive capped by `--archive-max-mb`, oldest frames removed, only files the service named itself. The guest archive is inside the VHDX, which already sits on the approved drive. | Partial: set the variable on the host in Stage 1. |
| F3 High | `/shell` runs anything; Default Switch is not production isolation | `/shell` is off unless started with `--enable-shell`. The PAUSE file now kills a running shell command (measured: under 1 s). | Partial: guest-to-LAN block at the host firewall and guest-only Git identity remain Stage 4 (tracker 14, 23). |
| F4 Medium | VM creation changed the host-wide Enhanced Session Mode | `New-Workbench.ps1` measures and reports the setting; changes it only with `-EnableEnhancedSession`. | Done, parse-checked. |
| F5 Medium | Driver crashed with a TypeError on API errors | Real bug (the word `kind` was passed twice). Fixed. | Done; live API error path still unverified. |

Decisions taken (final 18-10-2026): guest Windows = 90-day Enterprise evaluation ISO. VM disk = chosen only after preflight from measured suitable fixed drives; C: untouched; the SSD reserved for AVNI OS excluded; the 2 TB HDD is the candidate if healthy, roomy and fast enough. Guest-to-LAN block comes BEFORE the first network-connected boot, enforced on the host with Extended Port ACLs (section 8 step 4).

New house rule from the Architect: the Workbench is registered in AVNI Sentinel as **installed but OFF**. It never becomes a permanently running AVNI process. Tracker item 26.

Honest limits that stay: the token travels over plain HTTP on the host-guest link (acceptable for the pilot, both ends are on one PC). The viewer still takes Ravi's keyboard if he clicks inside it until worker card WC-03 is built.

## 5. Acceptance: the five demonstrations (measured, not screenshots)

1. Ravi types a document on Desktop 1 while the agent opens Chrome inside the workbench. His cursor position and keyboard focus are checked before and after.
2. The agent launches VS Code in the guest, edits a real file, saves, runs it.
3. Ravi watches the guest mouse click buttons of a real application in the viewer.
4. Host mouse position, focused window, and open files are unchanged (measured with the host's `GetCursorPos` and foreground window before and after the run).
5. The VM is shut down, restarted, and the installed tools and saved work are still there.

Each demo gets a line in `TRACKER.xlsx` with: measurement method, measured at, source. A demo is Done when Ravi says so.

## 6. Pilot operating rules

| Item | Decision |
|---|---|
| Name | AVNI Agent Workbench 01 |
| Host | Existing Windows 11 Pro PC |
| Isolation | Hyper-V Generation 2 VM, TPM, Secure Boot |
| Startup | Manual only. `AutomaticStartAction = Nothing` |
| RAM | Dynamic 2 to 8 GB, startup 6 GB. Preflight decides the actual number |
| CPU | 4 virtual processors |
| Disk | Dynamic VHDX, 200 GB ceiling, on an approved fixed drive, not C: |
| Network | Default Switch (NAT). No host shares |
| Agent | One agent at a time. Claude via `workbench_driver.py` first |
| Control | Guest-only mouse, keyboard, screen through `avni_hands.py` on TCP 7001 with a token |
| Pause / Stop | `PAUSE` file in the guest stops all input instantly. `STOP` file on the host ends the run after the current step |
| Record | Every screenshot and action in `%LOCALAPPDATA%\Avni\workbench\runs\<dd-mm-yyyy_HHMMSS>\` |
| Reports | `%LOCALAPPDATA%\Avni\reports\` |

## 7. Files in this folder

| Path | Purpose | Verified where |
|---|---|---|
| `TRACKER.xlsx` | Item / Status / Time taken / ETA / Notes. One sheet, latest status | n/a |
| `host/Preflight-Workbench.ps1` | Measures the PC: edition, CPU virtualization, Hyper-V state, RAM, disks, other hypervisors, GPU. Writes JSON + text report. Verdict READY / READY WITH CONDITIONS / NOT READY | PowerShell 7 parse only (Linux) |
| `host/New-Workbench.ps1` | Creates the VM with the network adapter DISCONNECTED (isolation first). Does not start it. Prints the human steps for Windows setup | PowerShell 7 parse only |
| `host/Set-WorkbenchNetworkIsolation.ps1` | Hyper-V Extended Port ACLs on the VM adapter: deny RFC1918 and link-local in and out; guest may reach the host (Default Switch gateway) only for DNS and hands-service replies, so host services such as SMB and RDP stay unreachable from the guest; internet open. Apply / Show / Remove / Connect / Disconnect; Connect refuses without the deny rules | PowerShell 7 parse only |
| `host/Test-WorkbenchIsolation.ps1` | Proves isolation from inside the guest over PowerShell Direct: LAN targets unreachable, internet reachable, DNS works. Verdict VERIFIED / FAILED / NOT MEASURED | PowerShell 7 parse only |
| `RESUME-LOCAL-SESSION.md` | Exact resume prompt, order of work and milestone ledger for the local session (Architect decisions 18-10-2026) | n/a |
| `host/Workbench.ps1` | Operations: Status, Start, Shutdown, Stop, Checkpoint, Checkpoints, Restore, Ip, Connect, Copy | PowerShell 7 parse only |
| `host/workbench_driver.py` | Claude computer-use loop mapped onto the guest service. Run-folder recording. STOP file. `--dry-run-model` exercises every action without the model | Linux, against the guest service in dry-run |
| `guest/avni_hands.py` | Guest-resident screen, mouse, keyboard and shell service. Token auth, client allow-list (host + localhost), `/shell` opt-in, PAUSE file, action log, capped screenshot archive | Linux dry-run and 54 unit tests. `SendInput` path unverified |
| `guest/keymap.py`, `guest/test_keymap.py`, `guest/test_hands_safety.py` | Key-name parsing, request validation, allow-list, archive retention, shell abort, unit-tested | Linux, `python3 -m unittest` |
| `guest/Install-Hands.ps1` | Installs the service inside the guest, firewall rule, logon task, measured health check | PowerShell 7 parse only |
| `cards/WORKER-CARDS.md` | Bounded cards for senior workers (ChatGPT Pro, Codex, Antigravity). No worker commits | n/a |

## 8. How the first run will go (local session on the PC, Claude runs these, not Ravi)

Order fixed by the Architect on 18-10-2026. Full prompt in `RESUME-LOCAL-SESSION.md`.

1. `host/Preflight-Workbench.ps1`, read-only, plain then elevated. Record the numbers in the tracker. Measure the 2 TB HDD health and speed.
2. Drive decision from the numbers: not C:, not the SSD reserved for AVNI OS. Set `AVNI_WORKBENCH_DATA` on that drive.
3. `host/New-Workbench.ps1 -VhdRoot <drive>\AVNI-VMs -IsoPath <evaluation ISO>` elevated. The adapter is created disconnected.
4. `host/Set-WorkbenchNetworkIsolation.ps1 -Action Apply`, then `-Action Show`, then `-Action Connect`. Before the first connected boot, never after.
5. Windows setup by hand in vmconnect (local account, offline). Then `Workbench.ps1 -Action Checkpoint -Label "clean install"`.
6. `host/Test-WorkbenchIsolation.ps1`. Required: ISOLATION VERIFIED. Nothing else proceeds until then.
7. Inside the guest: `winget install` Chrome, VS Code, Git, Python, PowerShell 7. Then `guest/Install-Hands.ps1` (no `-EnableShell` unless Ravi says so). Copy the token to the host once.
8. `host/Workbench.ps1 -Action Ip`, then `python host/workbench_driver.py --guest <ip> --dry-run-model` against the real guest. Required: every action ok.
9. First real task: `--task "Open Notepad, type today's date, save to Desktop as test.txt"`. Watch in vmconnect.
10. Acceptance demos 1 to 5. Then Sentinel registration as installed-but-OFF, ledger entry, mirror to `D:\AE Brain`.

## 9. Placement and visibility

This repository (`sehgalautoriders/avni`) is **public** on GitHub. Nothing in this folder contains a secret, a token or a credential, and nothing must ever be added that does. House law says everything we build lives in the AEBrain tree; this cloud session can only reach this repository, so the folder is here first. Tracker item 20: mirror or move it to `D:\AE Brain` in the next local session.
