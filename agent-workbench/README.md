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
| `host/New-Workbench.ps1` | Creates the VM. Does not start it. Prints the human steps for Windows setup | PowerShell 7 parse only |
| `host/Workbench.ps1` | Operations: Status, Start, Shutdown, Stop, Checkpoint, Checkpoints, Restore, Ip, Connect, Copy | PowerShell 7 parse only |
| `host/workbench_driver.py` | Claude computer-use loop mapped onto the guest service. Run-folder recording. STOP file. `--dry-run-model` exercises every action without the model | Linux, against the guest service in dry-run |
| `guest/avni_hands.py` | Guest-resident screen, mouse, keyboard and shell service. Token auth, PAUSE file, action log, screenshot archive | Linux dry-run only. `SendInput` path unverified |
| `guest/keymap.py`, `guest/test_keymap.py` | Key-name parsing and request validation, unit-tested | Linux, `python3 -m unittest` |
| `guest/Install-Hands.ps1` | Installs the service inside the guest, firewall rule, logon task, measured health check | PowerShell 7 parse only |
| `cards/WORKER-CARDS.md` | Bounded cards for senior workers (ChatGPT Pro, Codex, Antigravity). No worker commits | n/a |

## 8. How the first run will go (next local session, Claude runs these, not Ravi)

1. `host/Preflight-Workbench.ps1` on the host. Read the report. Record numbers in the tracker.
2. Owner decisions: licence path, VHDX drive.
3. `host/New-Workbench.ps1 -VhdRoot <approved drive>\AVNI-VMs -IsoPath <Windows ISO>` in an elevated PowerShell.
4. Human at the console: Windows setup inside vmconnect (about 40 minutes). Local account. No Microsoft account.
5. Inside the guest: `winget install` Chrome, VS Code, Git, Python, PowerShell 7. Then `guest/Install-Hands.ps1`.
6. Copy the token file from the guest to the host once (`Workbench.ps1 -Action Copy` goes host to guest; the token comes the other way through a shared clipboard in vmconnect or a one-time read).
7. `host/Workbench.ps1 -Action Ip`, then `python host/workbench_driver.py --guest <ip> --dry-run-model`. Measured: every action returns ok.
8. First real task: `--task "Open Notepad, type the date, save to Desktop as test.txt"`. Watch it in vmconnect.
9. Acceptance demos 1 to 5.

## 9. Placement and visibility

This repository (`sehgalautoriders/avni`) is **public** on GitHub. Nothing in this folder contains a secret, a token or a credential, and nothing must ever be added that does. House law says everything we build lives in the AEBrain tree; this cloud session can only reach this repository, so the folder is here first. Tracker item 20: mirror or move it to `D:\AE Brain` in the next local session.
