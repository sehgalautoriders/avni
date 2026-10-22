"""AVNI Agent Workbench - run one task inside the Workbench with Claude Code on the Claude SUBSCRIPTION (no API key).

What it does
    1. Measures the owner's own mouse position and foreground window (host) before the run.
    2. Starts the command-line Claude (claude.exe, signed in as sehgalautoriders2011@gmail.com, Max) in print mode with
       ONLY the Workbench tools (workbench_mcp.py): no shell, no files, no web on the host. Every tool call goes into the
       Workbench VM through the hands service.
    3. Streams Claude's steps to a log, then measures the owner's mouse and foreground window again.
    4. Prints whether the owner's desktop was untouched, and Claude's own final report.

Use
    python host\\workbench_task.py "Open Notepad, type today's date, save to Desktop as test.txt" [--max-turns 40]
    Before: host\\Workbench-Up.ps1 (elevated) once after every PC restart.
"""
from __future__ import annotations

import argparse
import ctypes
import ctypes.wintypes as wt
import glob
import json
import os
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Avni" / "workbench"
RUNS = BASE / "runs"
SYSTEM = (
    "You are an agent working INSIDE the AVNI Agent Workbench: a separate Windows 11 computer that belongs to you. "
    "You see and act ONLY through the mcp__workbench tools (screenshot, click, move, drag, scroll, type, key, wait, cursor). "
    "Coordinates are pixels of the LAST screenshot. Take a screenshot before deciding and after every action whose result "
    "you cannot yet see. If a dialog or welcome screen is in the way, close it. Work step by step like a careful person at "
    "the keyboard. When the task is done, take a final screenshot that proves it, then report in 3-6 short plain lines: "
    "what you did, what is on the screen now, and anything that did not work."
)


def find_claude() -> str:
    roaming = Path(os.environ.get("APPDATA", "")) / "Claude" / "claude-code"
    cands = []
    for exe in glob.glob(str(roaming / "*" / "*" / "claude.exe")):
        try:
            cands.append((tuple(int(x) for x in Path(exe).parents[1].name.split(".")), exe))
        except ValueError:
            pass
    if cands:
        return sorted(cands)[-1][1]
    local = Path.home() / ".local" / "bin" / "claude.exe"
    if local.exists():
        return str(local)
    raise SystemExit("claude.exe (Claude Code) not found on this PC")


def host_state() -> dict:
    u = ctypes.windll.user32
    pt = wt.POINT(); u.GetCursorPos(ctypes.byref(pt))
    hwnd = u.GetForegroundWindow()
    buf = ctypes.create_unicode_buffer(512); u.GetWindowTextW(hwnd, buf, 512)
    return {"at": datetime.now().strftime("%d-%m-%Y %H:%M:%S"), "cursor": [pt.x, pt.y], "foreground": hwnd, "title": buf.value}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("task")
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--model", default=None)
    a = ap.parse_args()

    RUNS.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now().strftime("%d-%m-%Y_%H%M%S")
    cfg = RUNS / f"mcp-config-{stamp}.json"
    cfg.write_text(json.dumps({"mcpServers": {"workbench": {"command": sys.executable, "args": [str(HERE / "workbench_mcp.py")], "env": {}}}}), encoding="utf-8")
    log = RUNS / f"task-{stamp}.log"
    cmd = [find_claude(), "-p", a.task, "--append-system-prompt", SYSTEM, "--mcp-config", str(cfg), "--strict-mcp-config",
           "--allowedTools", "mcp__workbench", "--disallowedTools", "Bash,PowerShell,Edit,Write,Read,Glob,Grep,WebFetch,WebSearch,Agent,NotebookEdit",
           "--output-format", "stream-json", "--verbose", "--max-turns", str(a.max_turns)]
    if a.model:
        cmd += ["--model", a.model]
    env = dict(os.environ)
    env.pop("ANTHROPIC_API_KEY", None)  # subscription only, never an API key

    before = host_state()
    print("OWNER DESKTOP BEFORE:", json.dumps(before, ensure_ascii=False), flush=True)
    t0 = time.time(); result = ""; calls = 0
    with open(log, "w", encoding="utf-8") as f:
        p = subprocess.Popen(cmd, cwd=str(RUNS), env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                             encoding="utf-8", errors="replace", creationflags=0x08000000)
        for line in p.stdout:
            f.write(line); f.flush()
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                print(line.rstrip()[:300], flush=True); continue
            if ev.get("type") == "assistant":
                for c in ev.get("message", {}).get("content", []):
                    if c.get("type") == "tool_use":
                        calls += 1
                        print(f"[{calls}] {c.get('name', '').replace('mcp__workbench__', '')} {json.dumps(c.get('input', {}), ensure_ascii=False)[:160]}", flush=True)
                    elif c.get("type") == "text" and c.get("text", "").strip():
                        print("claude:", c["text"].strip()[:400], flush=True)
            elif ev.get("type") == "result":
                result = ev.get("result") or ""
                print(f"RESULT: subtype={ev.get('subtype')} turns={ev.get('num_turns')} is_error={ev.get('is_error')}", flush=True)
        code = p.wait()
    after = host_state()
    print("OWNER DESKTOP AFTER: ", json.dumps(after, ensure_ascii=False), flush=True)
    same = before["cursor"] == after["cursor"] and before["foreground"] == after["foreground"]
    print(f"claude exit {code} in {int(time.time() - t0)} s, {calls} Workbench actions; owner's mouse and foreground window unchanged: {same}", flush=True)
    print(f"log: {log}", flush=True)
    print("FINAL REPORT:\n" + result, flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
