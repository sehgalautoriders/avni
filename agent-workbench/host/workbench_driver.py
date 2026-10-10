#!/usr/bin/env python3
"""
AVNI Agent Workbench - host-side driver.

Runs on the HOST (Ravi's Windows 11 Pro PC, or any machine that can reach the
guest). It never touches the host mouse or keyboard. It gives Claude the
computer-use toolset and forwards every action to the "hands" service that runs
INSIDE the Hyper-V guest (agent-workbench/guest/avni_hands.py, TCP 7001).

    Claude  --tool_use-->  this driver  --HTTP-->  guest hands  --SendInput-->  guest desktop
    Claude  <--screenshot--  this driver  <--PNG--  guest hands  <--ImageGrab--  guest screen

Every step (screenshot before, action, result, model text) is written to a run
folder so a failed run can be replayed and so the material becomes the AVNI
learning dataset later.

Safety:
  * STOP file  : <run-dir>/../STOP  - when it exists the loop ends after the current step.
  * PAUSE file : honoured by the guest service (423) - the driver waits and retries.
  * --max-steps and --max-minutes hard caps.
  * --checkpoint-before-run calls host/Workbench.ps1 -Action Checkpoint first (Windows only).

Status of this file: written and tested on Linux against avni_hands.py --dry-run.
The live Claude API path and the real guest path are CONFIGURED - UNVERIFIED
until run on the PC. Nothing here is a mock-up: the dry-run exercises the same
code paths with a generated screenshot.
"""
from __future__ import annotations

import argparse
import base64
import io
import json
import os
import platform
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

try:
    from PIL import Image
except ImportError:  # pragma: no cover - reported at startup instead
    Image = None  # type: ignore[assignment]

DEFAULT_MODEL = "claude-opus-5-5"
TOOLSET_TYPE = "computer_toolset_20260801"
TOOLSET_NAME = "computer"
# Long side of the screenshot handed to the model. Keeps every frame inside the
# model's image limits; the API does not downscale for the toolset.
MAX_IMAGE_LONG_SIDE = 1568

SYSTEM_PROMPT = """You are the AVNI Agent Workbench engineer. You are operating a Windows 11
computer that is YOURS: a Hyper-V virtual machine called "AVNI Agent Workbench 01".
Nothing you click or type reaches the owner's own desktop. Work like a careful
software engineer sitting at this machine: open applications, use the mouse and
keyboard, read the screen, run and test what you build, and fix what fails.

Rules of this machine:
- Take a screenshot before deciding and after any action whose result you cannot see yet.
- Prefer keyboard shortcuts for precision (Win+R, Ctrl+Shift+P in VS Code, Alt+Tab).
- Never type credentials you were not given. Never try to reach the owner's host PC,
  network shares or production databases; this machine has guest-only access.
- Save your work to disk often. Commit on the review branch you were told to use,
  never to main.
- When the task is finished, say plainly what you built, what you tested and how,
  and what is still owed. If you are blocked, say exactly what blocks you."""


# ----------------------------------------------------------------------------
# Hands client (guest side)
# ----------------------------------------------------------------------------
@dataclass
class Screen:
    width: int
    height: int


@dataclass
class HandsClient:
    base_url: str
    token: str
    timeout_s: float = 30.0

    def _req(self, method: str, path: str, body: dict | None = None) -> tuple[int, dict[str, str], bytes]:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(self.base_url + path, data=data, method=method)
        req.add_header("X-Avni-Token", self.token)
        if data is not None:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=self.timeout_s) as resp:
                return resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
        except urllib.error.HTTPError as e:
            return e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()

    def json(self, method: str, path: str, body: dict | None = None) -> dict[str, Any]:
        status, _, raw = self._req(method, path, body)
        try:
            payload = json.loads(raw.decode() or "{}")
        except json.JSONDecodeError:
            payload = {"raw": raw[:500].decode(errors="replace")}
        if status == 423:
            raise PausedError(payload.get("error", "paused"))
        if status >= 400:
            raise HandsError(f"{method} {path} -> HTTP {status}: {payload}")
        return payload

    def health(self) -> dict[str, Any]:
        return self.json("GET", "/health")

    def screenshot_png(self) -> tuple[bytes, Screen]:
        status, headers, raw = self._req("GET", "/screenshot?scale=1.0&format=png")
        if status == 423:
            raise PausedError("paused")
        if status >= 400:
            raise HandsError(f"screenshot -> HTTP {status}: {raw[:200]!r}")
        w = int(headers.get("x-screen-width", 0)) or None
        h = int(headers.get("x-screen-height", 0)) or None
        if not (w and h):
            if Image is None:
                raise HandsError("Pillow missing and the service sent no size headers")
            with Image.open(io.BytesIO(raw)) as im:
                w, h = im.size
        return raw, Screen(w, h)


class HandsError(RuntimeError):
    pass


class PausedError(RuntimeError):
    pass


# ----------------------------------------------------------------------------
# Image scaling between model space and guest space
# ----------------------------------------------------------------------------
@dataclass
class Scaler:
    """Model sees an image of at most MAX_IMAGE_LONG_SIDE px; the guest has the real
    resolution. Coordinates from the model are in the image space we last sent."""
    screen: Screen
    factor: float = 1.0  # image_px = guest_px * factor

    @classmethod
    def for_screen(cls, screen: Screen) -> "Scaler":
        long_side = max(screen.width, screen.height)
        factor = min(1.0, MAX_IMAGE_LONG_SIDE / long_side) if long_side else 1.0
        return cls(screen=screen, factor=factor)

    @property
    def image_size(self) -> tuple[int, int]:
        return (round(self.screen.width * self.factor), round(self.screen.height * self.factor))

    def to_guest(self, x: float, y: float) -> tuple[int, int]:
        gx = int(round(x / self.factor))
        gy = int(round(y / self.factor))
        gx = max(0, min(self.screen.width - 1, gx))
        gy = max(0, min(self.screen.height - 1, gy))
        return gx, gy

    def downscale(self, png: bytes) -> bytes:
        if self.factor >= 1.0 or Image is None:
            return png
        with Image.open(io.BytesIO(png)) as im:
            im = im.convert("RGB").resize(self.image_size, Image.LANCZOS)
            out = io.BytesIO()
            im.save(out, format="PNG", optimize=True)
            return out.getvalue()

    def zoom(self, png: bytes, region: list[int]) -> bytes:
        """region is [x0, y0, x1, y1] in IMAGE space. Returns a crop of the full-res
        screenshot resized so the model sees detail, at the same output size."""
        if Image is None:
            return png
        x0, y0 = self.to_guest(region[0], region[1])
        x1, y1 = self.to_guest(region[2], region[3])
        x0, x1 = sorted((x0, x1))
        y0, y1 = sorted((y0, y1))
        if x1 - x0 < 8 or y1 - y0 < 8:
            return self.downscale(png)
        with Image.open(io.BytesIO(png)) as im:
            crop = im.convert("RGB").crop((x0, y0, x1 + 1, y1 + 1))
            w, h = crop.size
            scale = min(MAX_IMAGE_LONG_SIDE / max(w, h), 4.0)
            crop = crop.resize((max(1, round(w * scale)), max(1, round(h * scale))), Image.LANCZOS)
            out = io.BytesIO()
            crop.save(out, format="PNG", optimize=True)
            return out.getvalue()


# ----------------------------------------------------------------------------
# Action mapping: computer toolset member -> hands endpoint(s)
# ----------------------------------------------------------------------------
BUTTON_OF = {
    "left_click": ("left", 1),
    "right_click": ("right", 1),
    "middle_click": ("middle", 1),
    "double_click": ("left", 2),
    "triple_click": ("left", 3),
}


def _coord(inp: dict[str, Any], key: str = "coordinate") -> tuple[float, float]:
    c = inp.get(key)
    if not (isinstance(c, (list, tuple)) and len(c) == 2):
        raise ValueError(f"'{key}' must be [x, y], got {c!r}")
    return float(c[0]), float(c[1])


def perform(hands: HandsClient, scaler: Scaler, name: str, inp: dict[str, Any]) -> tuple[str | None, bytes | None]:
    """Execute one toolset action. Returns (text_result, png_for_model)."""
    if name == "screenshot":
        png, _ = hands.screenshot_png()
        return None, scaler.downscale(png)

    if name == "zoom":
        png, _ = hands.screenshot_png()
        region = inp.get("region")
        if not (isinstance(region, (list, tuple)) and len(region) == 4):
            raise ValueError(f"'region' must be [x0, y0, x1, y1], got {region!r}")
        return None, scaler.zoom(png, [int(v) for v in region])

    if name == "cursor_position":
        cur = hands.json("GET", "/cursor")
        ix = round(cur["x"] * scaler.factor)
        iy = round(cur["y"] * scaler.factor)
        return f"X={ix},Y={iy}", None

    if name == "mouse_move":
        x, y = scaler.to_guest(*_coord(inp))
        hands.json("POST", "/mouse/move", {"x": x, "y": y})
        return "OK", None

    if name in BUTTON_OF:
        button, count = BUTTON_OF[name]
        x, y = scaler.to_guest(*_coord(inp))
        if inp.get("text"):
            # v1 of the guest service has no press-and-hold-across-requests; say so instead of half-doing it.
            raise ValueError("modifier-click (text + click) is not supported in v1; use left_mouse_down/up or a key action")
        hands.json("POST", "/mouse/click", {"x": x, "y": y, "button": button, "count": count})
        return "OK", None

    if name == "left_mouse_down":
        x, y = scaler.to_guest(*_coord(inp))
        hands.json("POST", "/mouse/down", {"x": x, "y": y, "button": "left"})
        return "OK", None

    if name == "left_mouse_up":
        x, y = scaler.to_guest(*_coord(inp))
        hands.json("POST", "/mouse/up", {"x": x, "y": y, "button": "left"})
        return "OK", None

    if name == "left_click_drag":
        sx, sy = scaler.to_guest(*_coord(inp, "start_coordinate"))
        ex, ey = scaler.to_guest(*_coord(inp))
        hands.json("POST", "/mouse/drag", {"from": {"x": sx, "y": sy}, "to": {"x": ex, "y": ey}, "button": "left", "steps": 25})
        return "OK", None

    if name == "scroll":
        x, y = scaler.to_guest(*_coord(inp))
        direction = str(inp.get("scroll_direction", "down")).lower()
        amount = int(inp.get("scroll_amount", 3))
        dx = dy = 0
        if direction == "up":
            dy = amount
        elif direction == "down":
            dy = -amount
        elif direction == "left":
            dx = -amount
        elif direction == "right":
            dx = amount
        else:
            raise ValueError(f"unknown scroll_direction {direction!r}")
        hands.json("POST", "/mouse/scroll", {"x": x, "y": y, "dx": dx, "dy": dy})
        return "OK", None

    if name == "type":
        text = inp.get("text")
        if not isinstance(text, str):
            raise ValueError("'text' must be a string")
        hands.json("POST", "/keyboard/type", {"text": text, "delay_ms": 8})
        return "OK", None

    if name == "key":
        combo = inp.get("text")
        if not isinstance(combo, str) or not combo.strip():
            raise ValueError("'text' must name a key or combo such as 'ctrl+s'")
        hands.json("POST", "/keyboard/key", {"combo": combo})
        return "OK", None

    if name == "hold_key":
        combo = inp.get("text")
        duration = float(inp.get("duration", 1.0))
        if not isinstance(combo, str) or not combo.strip():
            raise ValueError("'text' must name a key to hold")
        hands.json("POST", "/keyboard/hold", {"combo": combo, "duration_ms": int(duration * 1000)})
        return "OK", None

    if name == "wait":
        duration = float(inp.get("duration", 1.0))
        hands.json("POST", "/wait", {"ms": int(min(duration, 10.0) * 1000)})
        png, _ = hands.screenshot_png()
        return None, scaler.downscale(png)

    raise ValueError(f"unsupported computer action {name!r}")


# ----------------------------------------------------------------------------
# Run folder: the record of what the agent saw and did
# ----------------------------------------------------------------------------
@dataclass
class RunLog:
    root: Path
    step: int = 0
    events: list[dict[str, Any]] = field(default_factory=list)

    def __post_init__(self) -> None:
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "shots").mkdir(exist_ok=True)

    def event(self, kind: str, **data: Any) -> None:
        rec = {"t": datetime.now(timezone.utc).isoformat(timespec="milliseconds"), "step": self.step, "kind": kind, **data}
        self.events.append(rec)
        with (self.root / "events.jsonl").open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(rec, ensure_ascii=False) + "\n")

    def shot(self, png: bytes, label: str) -> str:
        name = f"{self.step:04d}-{label}.png"
        (self.root / "shots" / name).write_bytes(png)
        return name


# ----------------------------------------------------------------------------
# Main loop
# ----------------------------------------------------------------------------
def block_to_dict(block: Any) -> dict[str, Any]:
    """SDK content blocks -> plain dicts for the transcript on disk."""
    if hasattr(block, "model_dump"):
        return block.model_dump(exclude_none=True)
    return dict(block)


def run(args: argparse.Namespace) -> int:
    if Image is None:
        print("Pillow is not installed: pip install pillow", file=sys.stderr)
        return 2

    token_path = Path(args.token_file).expanduser()
    if not token_path.exists():
        print(f"token file not found: {token_path}\n"
              "Copy it from the guest (%LOCALAPPDATA%\\Avni\\workbench\\hands.token) once; it is created when avni_hands.py first starts.",
              file=sys.stderr)
        return 2
    token = token_path.read_text(encoding="utf-8").strip()

    hands = HandsClient(base_url=f"http://{args.guest}:{args.port}", token=token)
    try:
        health = hands.health()
    except Exception as e:  # noqa: BLE001 - any failure here is a hard stop
        print(f"cannot reach the guest hands service at {hands.base_url}: {e}", file=sys.stderr)
        return 3
    screen = Screen(int(health["screen"]["width"]), int(health["screen"]["height"]))
    scaler = Scaler.for_screen(screen)

    run_root = Path(args.run_dir) if args.run_dir else default_run_root()
    log = RunLog(run_root / datetime.now().strftime("%d-%m-%Y_%H%M%S"))
    stop_file = run_root / "STOP"
    log.event("start", guest=hands.base_url, health=health, model=args.model,
              image_size=scaler.image_size, scale_factor=scaler.factor, task=args.task, dry_run=bool(args.dry_run_model))
    print(f"run folder : {log.root}")
    print(f"guest      : {health.get('hostname')} {screen.width}x{screen.height} -> model sees {scaler.image_size[0]}x{scaler.image_size[1]}")
    print(f"stop file  : {stop_file}  (create it to stop after the current step)")

    if args.checkpoint_before_run:
        checkpoint(args.vm_name, log)

    if args.dry_run_model:
        return dry_run_actions(hands, scaler, log)

    try:
        import anthropic
    except ImportError:
        print("anthropic SDK missing: pip install anthropic", file=sys.stderr)
        return 2

    client = anthropic.Anthropic()
    messages: list[dict[str, Any]] = [{"role": "user", "content": [{"type": "text", "text": args.task}]}]
    started = time.monotonic()
    final_text = ""

    while True:
        log.step += 1
        if log.step > args.max_steps:
            log.event("stop", reason="max_steps")
            print(f"stopped: max steps ({args.max_steps}) reached")
            break
        if (time.monotonic() - started) > args.max_minutes * 60:
            log.event("stop", reason="max_minutes")
            print(f"stopped: {args.max_minutes} minutes reached")
            break
        if stop_file.exists():
            log.event("stop", reason="STOP file")
            print("stopped: STOP file present")
            break

        try:
            response = client.beta.messages.create(
                model=args.model,
                max_tokens=16000,
                system=SYSTEM_PROMPT,
                tools=[{"type": TOOLSET_TYPE}],
                messages=messages,
                betas=["server-side-fallback-2026-07-01"],
                fallbacks="default",
                output_config={"effort": args.effort},
            )
        except anthropic.RateLimitError as e:
            log.event("api_error", kind="rate_limit", message=str(e))
            print("rate limited; waiting 30 s")
            time.sleep(30)
            log.step -= 1
            continue
        except anthropic.APIStatusError as e:
            log.event("api_error", kind="status", status=e.status_code, message=str(e))
            print(f"API error {e.status_code}: {e}", file=sys.stderr)
            return 4
        except anthropic.APIConnectionError as e:
            log.event("api_error", kind="connection", message=str(e))
            print(f"connection error: {e}", file=sys.stderr)
            return 4

        content = [block_to_dict(b) for b in response.content]
        log.event("model", stop_reason=response.stop_reason, content=content,
                  usage=response.usage.model_dump() if hasattr(response.usage, "model_dump") else str(response.usage))
        messages.append({"role": "assistant", "content": response.content})

        for b in response.content:
            if getattr(b, "type", "") == "text" and b.text.strip():
                print(f"[{log.step}] {b.text.strip()}")
                final_text = b.text

        if response.stop_reason == "refusal":
            details = getattr(response, "stop_details", None)
            log.event("stop", reason="refusal", details=block_to_dict(details) if details else None)
            print("the model declined this request; see events.jsonl for the category")
            break
        if response.stop_reason in ("end_turn", "max_tokens"):
            log.event("stop", reason=response.stop_reason)
            break
        if response.stop_reason == "pause_turn":
            continue

        tool_uses = [b for b in response.content if getattr(b, "type", "") == "tool_use"]
        if not tool_uses:
            log.event("stop", reason=f"no tool use with stop_reason={response.stop_reason}")
            break

        results: list[dict[str, Any]] = []
        for tu in tool_uses:
            results.append(execute_tool_use(hands, scaler, log, tu.id, tu.name, dict(tu.input or {})))
        messages.append({"role": "user", "content": results})

    log.event("end", final_text=final_text)
    print(f"done. events: {log.root / 'events.jsonl'}")
    return 0


def execute_tool_use(hands: HandsClient, scaler: Scaler, log: RunLog, tool_use_id: str, name: str, inp: dict[str, Any]) -> dict[str, Any]:
    """One toolset action -> one tool_result (always echoing toolset_name)."""
    t0 = time.monotonic()
    for attempt in range(1, 61):  # paused guest: wait up to ~5 minutes
        try:
            text, png = perform(hands, scaler, name, inp)
            break
        except PausedError:
            if attempt == 1:
                print("guest is PAUSED (PAUSE file present); waiting...")
            time.sleep(5)
        except (HandsError, ValueError, KeyError, urllib.error.URLError) as e:
            took = round((time.monotonic() - t0) * 1000)
            log.event("action", name=name, input=inp, ok=False, error=str(e), took_ms=took)
            print(f"[{log.step}] {name} {inp} -> ERROR {e}")
            return {"type": "tool_result", "tool_use_id": tool_use_id, "toolset_name": TOOLSET_NAME,
                    "is_error": True, "content": f"{type(e).__name__}: {e}"}
    else:
        return {"type": "tool_result", "tool_use_id": tool_use_id, "toolset_name": TOOLSET_NAME,
                "is_error": True, "content": "guest stayed paused for 5 minutes"}

    took = round((time.monotonic() - t0) * 1000)
    rec: dict[str, Any] = {"name": name, "input": inp, "ok": True, "took_ms": took}
    content: Any
    if png is not None:
        rec["shot"] = log.shot(png, name)
        content = [{"type": "image", "source": {"type": "base64", "media_type": "image/png",
                                                 "data": base64.b64encode(png).decode()}}]
    else:
        content = text or "OK"
    log.event("action", **rec)
    print(f"[{log.step}] {name} {inp if name not in ('screenshot',) else ''} -> {'image' if png else content} ({took} ms)")
    return {"type": "tool_result", "tool_use_id": tool_use_id, "toolset_name": TOOLSET_NAME, "content": content}


def dry_run_actions(hands: HandsClient, scaler: Scaler, log: RunLog) -> int:
    """Exercise every mapping against the guest service without calling Claude.
    This is what we can measure on Linux today."""
    w, h = scaler.image_size
    script: list[tuple[str, dict[str, Any]]] = [
        ("screenshot", {}),
        ("mouse_move", {"coordinate": [w // 2, h // 2]}),
        ("left_click", {"coordinate": [w // 3, h // 3]}),
        ("double_click", {"coordinate": [w // 3, h // 3]}),
        ("right_click", {"coordinate": [10, 10]}),
        ("left_click_drag", {"start_coordinate": [100, 100], "coordinate": [300, 300]}),
        ("scroll", {"coordinate": [w // 2, h // 2], "scroll_direction": "down", "scroll_amount": 3}),
        ("type", {"text": "AVNI Agent Workbench नमस्ते"}),
        ("key", {"text": "ctrl+s"}),
        ("hold_key", {"text": "shift", "duration": 0.2}),
        ("cursor_position", {}),
        ("zoom", {"region": [0, 0, w // 2, h // 2]}),
        ("wait", {"duration": 0.2}),
        ("key", {"text": "not_a_key"}),  # must come back as is_error, not crash
    ]
    failures = 0
    for name, inp in script:
        log.step += 1
        res = execute_tool_use(hands, scaler, log, f"dry-{log.step}", name, inp)
        expect_error = name == "key" and inp.get("text") == "not_a_key"
        if bool(res.get("is_error")) != expect_error:
            failures += 1
    log.event("end", dry_run=True, failures=failures)
    print(f"dry run finished: {len(script)} actions, {failures} unexpected failures, record in {log.root}")
    return 0 if failures == 0 else 1


def checkpoint(vm_name: str, log: RunLog) -> None:
    if platform.system() != "Windows":
        log.event("checkpoint", skipped="not on Windows host")
        return
    script = Path(__file__).with_name("Workbench.ps1")
    cmd = ["powershell.exe", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(script),
           "-Action", "Checkpoint", "-Name", vm_name, "-Label", "before-agent-run"]
    proc = subprocess.run(cmd, capture_output=True, text=True, timeout=300)
    log.event("checkpoint", code=proc.returncode, stdout=proc.stdout[-2000:], stderr=proc.stderr[-2000:])
    print(f"checkpoint: exit {proc.returncode}")


def default_run_root() -> Path:
    if platform.system() == "Windows":
        return Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Avni" / "workbench" / "runs"
    return Path.home() / ".avni" / "workbench" / "runs"


def default_token_path() -> str:
    if platform.system() == "Windows":
        return str(Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Avni" / "workbench" / "hands.token")
    return str(Path.home() / ".avni" / "workbench" / "hands.token")


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description="Drive the AVNI Agent Workbench guest with Claude computer use.")
    p.add_argument("--guest", required=True, help="IP of the guest (Workbench.ps1 -Action Ip prints it), or 127.0.0.1 for a local dry run")
    p.add_argument("--port", type=int, default=7001)
    p.add_argument("--token-file", default=default_token_path(), help="copy of the guest's hands.token")
    p.add_argument("--task", default="Take a screenshot and describe what is on the screen.", help="what the agent must do")
    p.add_argument("--model", default=DEFAULT_MODEL)
    p.add_argument("--effort", default="high", choices=["low", "medium", "high", "xhigh", "max"])
    p.add_argument("--max-steps", type=int, default=60)
    p.add_argument("--max-minutes", type=float, default=45)
    p.add_argument("--run-dir", default=None, help="where run folders go (default %%LOCALAPPDATA%%\\Avni\\workbench\\runs)")
    p.add_argument("--vm-name", default="AVNI Agent Workbench 01")
    p.add_argument("--checkpoint-before-run", action="store_true", help="Hyper-V checkpoint via Workbench.ps1 before the first action")
    p.add_argument("--dry-run-model", action="store_true", help="no Claude call; exercise every action against the guest service")
    args = p.parse_args(argv)
    return run(args)


if __name__ == "__main__":
    sys.exit(main())
