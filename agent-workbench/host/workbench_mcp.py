"""AVNI Agent Workbench - the Workbench's eyes and hands for the command-line Claude (Claude Code).

What it is
    A small MCP tool server (MCP = the plug-in standard Claude Code uses for extra tools). It speaks to the guest
    "hands" service over the host-only link (TCP 7001) and gives Claude Code these tools: screenshot, click, move,
    drag, scroll, type, key, wait, cursor. Claude Code then works INSIDE the Workbench with its own mouse and keyboard.

Why
    Ravi 22-10-2026: no Anthropic API key (too costly); use the Claude subscription through the command-line Claude,
    signed in as sehgalautoriders2011@gmail.com (Max). The API-based workbench_driver.py stays for later; this file is
    the subscription road. Same hands service, same token, same guest, no extra cost per call.

How it runs (Claude Code starts it; nobody runs it by hand)
    claude -p "<task>" --mcp-config workbench-mcp.json --strict-mcp-config --allowedTools "mcp__workbench"
    workbench-mcp.json: {"mcpServers": {"workbench": {"command": "python", "args": ["<this file>"],
                         "env": {}}}}  (guest address: guest-ip.txt from Workbench-Up.ps1)

Coordinates
    Screenshots are scaled to at most 1280 x 720 before Claude sees them, so the picture is never resized again on the
    way. Every x, y Claude sends is in that picture; this server maps it back to the real guest screen.

Safety
    Only the guest is touched: the hands service runs inside the VM and refuses every client except the host.
    The host's own mouse and keyboard are never used. The token is read from %LOCALAPPDATA%\\Avni\\workbench\\hands.token
    and never printed. Every call is logged to %LOCALAPPDATA%\\Avni\\workbench\\runs\\mcp-<date>.log.
"""
from __future__ import annotations

import base64
import io
import json
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

try:
    from PIL import Image
except ImportError:  # the server still works, at full size
    Image = None

BASE = Path(os.environ.get("LOCALAPPDATA", str(Path.home()))) / "Avni" / "workbench"
def _guest_ip() -> str:
    """WB_GUEST, else the address Workbench-Up.ps1 wrote after the last PC restart."""
    if os.environ.get("WB_GUEST"):
        return os.environ["WB_GUEST"]
    try:
        return (BASE / "guest-ip.txt").read_text(encoding="ascii").strip()
    except OSError:
        return "172.26.35.77"


GUEST = _guest_ip()
PORT = int(os.environ.get("WB_PORT", "7001"))
TOKEN_FILE = Path(os.environ.get("WB_TOKEN_FILE", str(BASE / "hands.token")))
MAX_W, MAX_H = 1280, 720
LOG = BASE / "runs" / f"mcp-{datetime.now():%d-%m-%Y}.log"
SHOTS = BASE / "runs" / "mcp-shots"

_scale = [1.0]          # guest pixels per picture pixel, set by the last screenshot
_screen = [1920, 1080]  # real guest size from the last screenshot


def log(msg: str) -> None:
    try:
        LOG.parent.mkdir(parents=True, exist_ok=True)
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(f"{datetime.now():%d-%m-%Y %H:%M:%S} {msg}\n")
    except OSError:
        pass


def hands(method: str, path: str, body: dict | None = None, raw: bool = False):
    token = TOKEN_FILE.read_text(encoding="utf-8").strip()
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"http://{GUEST}:{PORT}{path}", data=data, method=method)
    req.add_header("X-Avni-Token", token)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            payload = r.read()
            return (payload, dict(r.headers)) if raw else json.loads(payload.decode() or "{}")
    except urllib.error.HTTPError as e:
        detail = e.read()[:300].decode(errors="replace")
        if e.code == 423:
            raise RuntimeError("PAUSED: the Workbench owner paused all input (PAUSE file). Stop and report.")
        raise RuntimeError(f"{method} {path} -> HTTP {e.code}: {detail}")


def to_guest(x, y) -> tuple[int, int]:
    gx = int(round(float(x) * _scale[0]))
    gy = int(round(float(y) * _scale[0]))
    return max(0, min(_screen[0] - 1, gx)), max(0, min(_screen[1] - 1, gy))


def screenshot() -> list[dict]:
    png, headers = hands("GET", "/screenshot?scale=1.0&format=png", raw=True)
    w = int(headers.get("X-Screen-Width") or headers.get("x-screen-width") or 0)
    h = int(headers.get("X-Screen-Height") or headers.get("x-screen-height") or 0)
    out = png
    if Image is not None:
        im = Image.open(io.BytesIO(png))
        w, h = w or im.size[0], h or im.size[1]
        f = max(w / MAX_W, h / MAX_H, 1.0)
        if f > 1.0:
            im = im.convert("RGB").resize((int(w / f), int(h / f)), Image.LANCZOS)
        buf = io.BytesIO(); im.save(buf, "PNG", optimize=True); out = buf.getvalue()
        _scale[0] = f
        pw, ph = im.size
    else:
        _scale[0] = 1.0
        pw, ph = w, h
    _screen[0], _screen[1] = w or _screen[0], h or _screen[1]
    try:
        SHOTS.mkdir(parents=True, exist_ok=True)
        (SHOTS / f"{datetime.now():%d-%m-%Y_%H%M%S}.png").write_bytes(out)
    except OSError:
        pass
    return [{"type": "image", "data": base64.b64encode(out).decode(), "mimeType": "image/png"},
            {"type": "text", "text": f"Workbench screen {pw}x{ph} (real {w}x{h}). Use x, y in this picture."}]


def call(name: str, a: dict) -> list[dict]:
    def ok(t="OK"):
        return [{"type": "text", "text": t}]
    if name == "screenshot":
        return screenshot()
    if name == "click":
        x, y = to_guest(a["x"], a["y"])
        hands("POST", "/mouse/click", {"x": x, "y": y, "button": a.get("button", "left"), "count": int(a.get("count", 1))})
        return ok()
    if name == "move":
        x, y = to_guest(a["x"], a["y"])
        hands("POST", "/mouse/move", {"x": x, "y": y}); return ok()
    if name == "drag":
        sx, sy = to_guest(a["from_x"], a["from_y"]); ex, ey = to_guest(a["to_x"], a["to_y"])
        hands("POST", "/mouse/drag", {"from": {"x": sx, "y": sy}, "to": {"x": ex, "y": ey}, "button": "left", "steps": 25}); return ok()
    if name == "scroll":
        x, y = to_guest(a["x"], a["y"]); n = int(a.get("amount", 3)); d = str(a.get("direction", "down")).lower()
        dx, dy = {"up": (0, n), "down": (0, -n), "left": (-n, 0), "right": (n, 0)}[d]
        hands("POST", "/mouse/scroll", {"x": x, "y": y, "dx": dx, "dy": dy}); return ok()
    if name == "type":
        hands("POST", "/keyboard/type", {"text": str(a["text"]), "delay_ms": 8}); return ok()
    if name == "key":
        hands("POST", "/keyboard/key", {"combo": str(a["combo"])}); return ok()
    if name == "wait":
        time.sleep(min(float(a.get("seconds", 1)), 10.0)); return ok()
    if name == "cursor":
        c = hands("GET", "/cursor")
        return ok(f"cursor at x={int(c.get('x', 0) / _scale[0])}, y={int(c.get('y', 0) / _scale[0])} (picture pixels)")
    raise ValueError(f"unknown tool {name}")


def _num(desc):
    return {"type": "number", "description": desc}


TOOLS = [
    {"name": "screenshot", "description": "See the Workbench screen now (its own Windows desktop, not the owner's). Take one before deciding and after any action whose result you cannot yet see.", "inputSchema": {"type": "object", "properties": {}}},
    {"name": "click", "description": "Click in the Workbench at x, y (pixels of the last screenshot). count 2 = double-click; button left/right/middle.", "inputSchema": {"type": "object", "properties": {"x": _num("x"), "y": _num("y"), "button": {"type": "string", "enum": ["left", "right", "middle"]}, "count": {"type": "integer", "minimum": 1, "maximum": 3}}, "required": ["x", "y"]}},
    {"name": "move", "description": "Move the Workbench mouse to x, y.", "inputSchema": {"type": "object", "properties": {"x": _num("x"), "y": _num("y")}, "required": ["x", "y"]}},
    {"name": "drag", "description": "Drag with the left button from one point to another.", "inputSchema": {"type": "object", "properties": {"from_x": _num(""), "from_y": _num(""), "to_x": _num(""), "to_y": _num("")}, "required": ["from_x", "from_y", "to_x", "to_y"]}},
    {"name": "scroll", "description": "Scroll at x, y.", "inputSchema": {"type": "object", "properties": {"x": _num("x"), "y": _num("y"), "direction": {"type": "string", "enum": ["up", "down", "left", "right"]}, "amount": {"type": "integer", "minimum": 1, "maximum": 20}}, "required": ["x", "y", "direction"]}},
    {"name": "type", "description": "Type text into the focused field of the Workbench (any language).", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}},
    {"name": "key", "description": "Press a key or combination in the Workbench, e.g. 'enter', 'ctrl+s', 'win+r', 'alt+f4'.", "inputSchema": {"type": "object", "properties": {"combo": {"type": "string"}}, "required": ["combo"]}},
    {"name": "wait", "description": "Wait up to 10 seconds for the Workbench to finish something.", "inputSchema": {"type": "object", "properties": {"seconds": _num("seconds, max 10")}}},
    {"name": "cursor", "description": "Where the Workbench mouse is now.", "inputSchema": {"type": "object", "properties": {}}},
]


def send(obj: dict) -> None:
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()


def main() -> int:
    log(f"server start, guest {GUEST}:{PORT}")
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        mid, method = msg.get("id"), msg.get("method")
        if mid is None:
            continue  # notifications need no answer
        try:
            if method == "initialize":
                res = {"protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
                       "capabilities": {"tools": {}}, "serverInfo": {"name": "avni-workbench", "version": "1.0.0"}}
            elif method == "tools/list":
                res = {"tools": TOOLS}
            elif method == "tools/call":
                p = msg.get("params", {}); name = p.get("name"); args = p.get("arguments") or {}
                t0 = time.time()
                try:
                    content = call(name, args); res = {"content": content}
                    log(f"{name} {json.dumps(args, ensure_ascii=False)[:200]} -> OK {int((time.time() - t0) * 1000)} ms")
                except Exception as e:  # report the error to Claude, keep serving
                    res = {"content": [{"type": "text", "text": f"ERROR: {e}"}], "isError": True}
                    log(f"{name} {json.dumps(args, ensure_ascii=False)[:200]} -> ERROR {e}")
            elif method == "ping":
                res = {}
            else:
                send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"no method {method}"}}); continue
            send({"jsonrpc": "2.0", "id": mid, "result": res})
        except Exception as e:
            send({"jsonrpc": "2.0", "id": mid, "error": {"code": -32603, "message": str(e)}})
    return 0


if __name__ == "__main__":
    sys.exit(main())
