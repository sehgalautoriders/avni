# AVNI Agent Workbench - worker cards

Cards for senior workers (ChatGPT Pro, Codex, Antigravity, local models). Each card is bounded. Claude reviews the diff, integrates, verifies on the real runtime and commits. Workers never commit.

Common lines for every card:

- Repo folder: `agent-workbench/`. Read `README.md` first, letter by letter.
- DO NOT COMMIT, MERGE, REBASE, PUSH OR CHANGE RELEASE HISTORY.
- Touch only the allowed files. Do not reformat files you were not asked to change.
- No secrets, tokens, personal paths or credentials in any file.
- Evidence means measured output pasted into your report, not a description.

---

## WC-01 - Run the preflight on the real PC and fill the tracker (local Claude session, not a worker)

Kept by Claude. Listed so nobody picks it up. Runs `host/Preflight-Workbench.ps1`, reads the report, fills tracker items 2, 3, 4 with measured numbers.

---

## WC-02 - Accessibility tree endpoint `/uia` in the guest service

Allowed files: `guest/avni_hands.py` (only the `/uia` handler and a new module import), new file `guest/uia_reader.py`, new file `guest/test_uia_reader.py`.
Forbidden: everything else, especially the HTTP contract of the other endpoints and `keymap.py`.

Task: replace the 501 stub with a real reader using `pywinauto` (UIA backend) or `comtypes` + UIAutomationCore. `GET /uia?depth=3&hwnd=<optional>` returns the foreground window's element tree: control type, name, automation id, bounding rectangle, enabled, focused, up to `depth`. Cap at 500 elements, flag `truncated`. Pure tree-walking and JSON shaping goes into `uia_reader.py` and is unit-tested with a fake tree on Linux.

Definition of Done: `/uia` returns JSON for Notepad with its menu bar and edit control named; unit tests pass with `python3 -m unittest`; response under 300 ms for depth 3 on Notepad (measured, pasted).

---

## WC-03 - Read-only live viewer window for Desktop 1

Allowed files: new folder `host/viewer/` only (any files inside), plus one line in `README.md` section 7 adding the row.
Forbidden: every other file.

Task: a small native Windows window (WPF or WinUI 3, C#, dark and light mode following the system) that polls `GET /screenshot?scale=0.5` from the guest service at up to 5 frames per second and displays it, with a status bar: guest hostname, screen size, frames per second, last action from the run folder's `events.jsonl`, PAUSED badge when the service reports `paused: true`. It never sends input. It has two buttons only: `Pause agent` (creates the PAUSE file in the guest through `POST /shell`), `Resume`. Window title: `AVNI Agent Workbench 01 - viewer`. No terminal window. No localhost web page.

Definition of Done: runs on the host, shows the guest desktop changing live while `workbench_driver.py` runs; clicking inside the viewer does nothing to the guest (measured: guest cursor position unchanged, read from `/cursor` before and after ten clicks); CPU under 5 percent on the host at 5 fps (Task Manager, pasted).

---

## WC-04 - Golden image export and clone script

Allowed files: new `host/Export-Workbench.ps1`, new `host/Clone-Workbench.ps1`.
Forbidden: everything else.

Task: `Export-Workbench.ps1` shuts the VM down gracefully, runs `Export-VM` to an approved folder, writes a manifest JSON (name, date dd-MM-yyyy, VHDX size, sha256 of the exported VHDX). `Clone-Workbench.ps1 -Source <export folder> -Name 'AVNI Agent Workbench 02'` does `Import-VM -Copy -GenerateNewId`, renames, gives it its own MAC, sets `AutomaticStartAction Nothing`, and prints a measured summary. Note in comments that a Windows guest cloned without sysprep keeps its machine SID; acceptable for isolated agents, state it.

Definition of Done: parse-clean under PowerShell 7; dry logic reviewed by Claude; real run only after acceptance demos pass.

---

## WC-05 - Learning dataset packer

Allowed files: new `host/pack_run.py`, new `host/test_pack_run.py`.
Forbidden: everything else.

Task: read a run folder (`events.jsonl` + `shots/`) and produce one `run.html` (dark and light, tab UI, one panel visible at a time, table not cards) listing every step: time, action, input, result, thumbnail of the screenshot before and after, model text. Also `run-summary.json`: steps, actions by type, errors, total time, tokens from `usage` where present. Pure Python standard library plus Pillow for thumbnails. Unit-test the summary on a fixture run folder.

Definition of Done: tests pass; `run.html` opens in Chrome and shows the dry-run fixture correctly; no network calls.
