#!/usr/bin/env python3
"""AVNI Agent Workbench - guest-resident "hands" service (v0.1.0).

Runs INSIDE the Windows 11 Hyper-V guest and gives an AI agent a screen, mouse
and keyboard that belong to the VM only. A host-side driver calls it over HTTP.

Conventions
-----------
* JSON in, JSON out. Every request needs header ``X-Avni-Token``.
* Coordinates are screenshot pixels of the PRIMARY screen (the process is made
  DPI-aware at startup so pixels match). Valid range: 0..width-1, 0..height-1.
* Scroll: positive ``dy`` scrolls UP (WHEEL_DELTA > 0, wheel away from the user),
  negative ``dy`` scrolls DOWN. Positive ``dx`` scrolls RIGHT. One unit is one
  wheel notch (120 delta). Example: {"x":640,"y":360,"dx":0,"dy":-3} scrolls
  down three notches.
* Every input endpoint answers {"ok":true,"action":<path with '/'->'_'>,"took_ms":n}
  plus endpoint specific fields, and "dry_run":true in dry-run mode.
* Input endpoints (/mouse/*, /keyboard/*, /shell) answer 423 {"error":"paused"}
  while the PAUSE file exists and 429 above 50 actions/second.

Run:  python avni_hands.py [--host 0.0.0.0] [--port 7001] [--token-file PATH]
                           [--archive-dir DIR] [--dry-run]
"""
from __future__ import annotations

import argparse
import base64
import hmac
import io
import json
import os
import secrets
import socket
import subprocess
import sys
import threading
import time
import traceback
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable, Optional, Protocol
from urllib.parse import parse_qs, urlsplit

import keymap

try:  # Pillow is a hard requirement for /screenshot only
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover - reported at request time
    Image = ImageDraw = ImageFont = None  # type: ignore[assignment]

VERSION = "0.1.0"
SERVICE = "avni-hands"
DEFAULT_PORT = 7001
IS_WINDOWS = sys.platform == "win32"

MAX_BODY_BYTES = 1_000_000
MAX_INPUT_PER_SEC = 50
MAX_WAIT_MS = 10_000
MAX_TEXT_CHARS = 10_000
SHELL_OUTPUT_CAP = 200 * 1024
BUTTONS = ("left", "right", "middle")
DRY_RUN_SIZE = (1280, 720)


# =============================================================================
# Small shared types and pure helpers (unit-tested on any OS)
# =============================================================================
@dataclass(frozen=True)
class ScreenInfo:
    """Primary-screen size plus the virtual-desktop rectangle used to normalise
    absolute mouse coordinates (0 means "same as the primary screen")."""

    width: int
    height: int
    v_left: int = 0
    v_top: int = 0
    v_width: int = 0
    v_height: int = 0

    @property
    def vw(self) -> int:
        return self.v_width or self.width

    @property
    def vh(self) -> int:
        return self.v_height or self.height


@dataclass(frozen=True)
class ServerConfig:
    """The only shared configuration object (immutable)."""

    host: str
    port: int
    token: str = field(repr=False)
    dry_run: bool
    token_path: Path
    log_path: Path
    pause_path: Path
    archive_dir: Optional[Path] = None

    def is_paused(self) -> bool:
        try:
            return self.pause_path.exists()
        except OSError:
            return False


class ApiError(Exception):
    """An error that maps straight to an HTTP status and a JSON body."""

    def __init__(self, status: int, message: str, headers: Optional[dict[str, str]] = None, **extra: Any) -> None:
        super().__init__(message)
        self.status = status
        self.message = message
        self.headers = headers or {}
        self.extra = extra

    def to_response(self) -> "Response":
        return Response(self.status, payload={"error": self.message, **self.extra}, headers=dict(self.headers))


class ActionAborted(Exception):
    """The PAUSE file appeared while an action was running."""


@dataclass
class Response:
    status: int = 200
    payload: Optional[dict[str, Any]] = None
    body: Optional[bytes] = None
    content_type: str = "application/json; charset=utf-8"
    headers: dict[str, str] = field(default_factory=dict)


def iso_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


def default_base_dir() -> Path:
    root = os.environ.get("LOCALAPPDATA")
    if root:
        return Path(root) / "Avni" / "workbench"
    return Path.home() / ".local" / "share" / "Avni" / "workbench"


def token_matches(expected: str, supplied: Optional[str]) -> bool:
    if not supplied:
        return False
    return hmac.compare_digest(expected.encode("utf-8"), supplied.encode("utf-8", "replace"))


def load_or_create_token(path: Path) -> tuple[str, bool]:
    """Return (token, created). Generates 32 random bytes, urlsafe, on first run."""
    if path.exists():
        return _read_token(path), False
    path.parent.mkdir(parents=True, exist_ok=True)
    token = secrets.token_urlsafe(32)
    try:
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        return _read_token(path), False
    with os.fdopen(fd, "w", encoding="utf-8") as fh:
        fh.write(token + "\n")
    return token, True


def _read_token(path: Path) -> str:
    token = path.read_text(encoding="utf-8-sig").strip()
    if not token:
        raise SystemExit(f"token file {path} is empty; delete it to have a new token generated")
    return token


class RateLimiter:
    """Sliding one-second window; thread-safe."""

    def __init__(self, max_per_sec: int, clock: Callable[[], float] = time.monotonic) -> None:
        self._max = max_per_sec
        self._clock = clock
        self._hits: deque[float] = deque()
        self._lock = threading.Lock()

    def allow(self) -> bool:
        now = self._clock()
        with self._lock:
            while self._hits and now - self._hits[0] >= 1.0:
                self._hits.popleft()
            if len(self._hits) >= self._max:
                return False
            self._hits.append(now)
            return True


# ---- request validation -----------------------------------------------------
_REQUIRED: Any = object()


def as_int(value: Any, name: str, lo: int, hi: int) -> int:
    """Strict-ish integer: bool and non-integral floats are rejected; 12.0 is accepted."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ApiError(400, f"{name} must be an integer", field=name)
    if isinstance(value, float):
        if not value.is_integer():
            raise ApiError(400, f"{name} must be an integer", field=name)
        value = int(value)
    if not lo <= value <= hi:
        raise ApiError(400, f"{name} must be between {lo} and {hi}", field=name, value=value)
    return value


def get_int(body: dict[str, Any], key: str, lo: int, hi: int, default: Any = _REQUIRED) -> int:
    if key not in body or body[key] is None:
        if default is _REQUIRED:
            raise ApiError(400, f"missing required field: {key}", field=key)
        return default
    return as_int(body[key], key, lo, hi)


def validate_xy(obj: dict[str, Any], screen: ScreenInfo) -> tuple[int, int]:
    """x and y must be ints inside the primary screen."""
    return get_int(obj, "x", 0, screen.width - 1), get_int(obj, "y", 0, screen.height - 1)


def validate_point(body: dict[str, Any], key: str, screen: ScreenInfo) -> tuple[int, int]:
    sub = body.get(key)
    if not isinstance(sub, dict):
        raise ApiError(400, f"{key} must be an object with x and y", field=key)
    try:
        return validate_xy(sub, screen)
    except ApiError as exc:
        exc.extra["field"] = f"{key}.{exc.extra.get('field', '')}".rstrip(".")
        raise


def validate_button(body: dict[str, Any], default: str = "left") -> str:
    button = body.get("button", default)
    if not isinstance(button, str) or button.lower() not in BUTTONS:
        raise ApiError(400, f"button must be one of {', '.join(BUTTONS)}", field="button")
    return button.lower()


def validate_count(body: dict[str, Any]) -> int:
    return get_int(body, "count", 1, 3, default=1)


def parse_json_body(raw: bytes) -> dict[str, Any]:
    if not raw.strip():
        return {}
    try:
        data = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ApiError(400, f"malformed JSON: {exc}") from None
    if not isinstance(data, dict):
        raise ApiError(400, "JSON body must be an object")
    return data


def parse_scale(text: str) -> float:
    try:
        scale = float(text)
    except ValueError:
        raise ApiError(400, "scale must be a number between 0.1 and 1.0", field="scale") from None
    if not 0.1 <= scale <= 1.0:  # also rejects NaN
        raise ApiError(400, "scale must be between 0.1 and 1.0", field="scale")
    return scale


def normalize_abs(x: int, y: int, screen: ScreenInfo) -> tuple[int, int]:
    """Pixel -> 0..65535 for MOUSEEVENTF_ABSOLUTE|VIRTUALDESK."""
    nx = round((x - screen.v_left) * 65535 / max(screen.vw - 1, 1))
    ny = round((y - screen.v_top) * 65535 / max(screen.vh - 1, 1))
    return max(0, min(65535, nx)), max(0, min(65535, ny))


def cap_output(data: bytes, limit: int, encoding: str = "utf-8") -> tuple[str, bool]:
    truncated = len(data) > limit
    try:
        text = data[:limit].decode(encoding, "replace")
    except LookupError:
        text = data[:limit].decode("utf-8", "replace")
    return text, truncated


def trim_for_log(value: Any, limit: int = 500) -> Any:
    if isinstance(value, str):
        return value if len(value) <= limit else f"{value[:limit]}...[+{len(value) - limit} chars]"
    if isinstance(value, dict):
        return {k: trim_for_log(v, limit) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [trim_for_log(v, limit) for v in value[:50]]
    return value


# ---- "is this really a Hyper-V guest?" --------------------------------------
def model_indicates_hyperv_guest(model: Optional[str]) -> bool:
    return bool(model) and "virtual machine" in model.lower()  # type: ignore[union-attr]


def _probe_registry() -> Optional[str]:
    try:
        import winreg

        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, r"HARDWARE\DESCRIPTION\System\BIOS") as key:
            return str(winreg.QueryValueEx(key, "SystemProductName")[0])
    except (ImportError, OSError):
        return None


def _run_quiet(cmd: list[str]) -> Optional[str]:
    try:
        done = subprocess.run(cmd, capture_output=True, text=True, timeout=25, stdin=subprocess.DEVNULL,
                              creationflags=0x08000000 if IS_WINDOWS else 0)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout if done.returncode == 0 else None


def _probe_cim() -> Optional[str]:
    out = _run_quiet(["powershell", "-NoProfile", "-NonInteractive", "-Command",
                      "(Get-CimInstance Win32_ComputerSystem).Model"])
    return out.strip() if out and out.strip() else None


def _probe_wmic() -> Optional[str]:
    out = _run_quiet(["wmic", "computersystem", "get", "model", "/value"])
    for line in (out or "").splitlines():
        if line.strip().lower().startswith("model="):
            return line.split("=", 1)[1].strip() or None
    return None


def looks_like_hyperv_guest(probes: Optional[list[Callable[[], Optional[str]]]] = None) -> tuple[bool, str]:
    """First probe that returns a machine model decides. The vmicheartbeat service
    key is deliberately NOT used: it also exists on a Hyper-V HOST."""
    for probe in probes or [_probe_registry, _probe_cim, _probe_wmic]:
        model = probe()
        if model:
            return model_indicates_hyperv_guest(model), f"machine model {model!r} via {getattr(probe, '__name__', 'probe')}"
    return False, "could not read the machine model"


# =============================================================================
# Logging
# =============================================================================
class ActionLog:
    """One JSON object per line, append-only, thread-safe, never raises."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = threading.Lock()

    def write(self, record: dict[str, Any]) -> None:
        line = json.dumps(record, ensure_ascii=False, default=str)
        try:
            with self._lock:
                self.path.parent.mkdir(parents=True, exist_ok=True)
                with open(self.path, "a", encoding="utf-8", errors="backslashreplace") as fh:
                    fh.write(line + "\n")
        except OSError:
            pass


# =============================================================================
# Backends
# =============================================================================
class Backend(Protocol):
    dry_run: bool

    def screen_info(self) -> ScreenInfo: ...
    def cursor_pos(self) -> tuple[int, int]: ...
    def foreground_window(self) -> dict[str, Any]: ...
    def screenshot(self) -> Any: ...
    def mouse_move(self, x: int, y: int) -> None: ...
    def mouse_button(self, x: int, y: int, button: str, down: bool) -> None: ...
    def mouse_click(self, x: int, y: int, button: str, count: int) -> None: ...
    def mouse_drag(self, x1: int, y1: int, x2: int, y2: int, button: str, steps: int, hold_ms: int) -> None: ...
    def mouse_scroll(self, x: Optional[int], y: Optional[int], dx: int, dy: int) -> None: ...
    def type_text(self, text: str, delay_ms: int) -> int: ...
    def key_combo(self, tokens: list[keymap.Token]) -> None: ...
    def key_hold(self, tokens: list[keymap.Token], duration_ms: int) -> None: ...


def encode_png(img: Any, level: int = 3) -> bytes:
    buf = io.BytesIO()
    img.save(buf, "PNG", compress_level=level)
    return buf.getvalue()


def _need_pillow() -> None:
    if Image is None:
        raise RuntimeError("Pillow is not installed (pip install pillow)")


def _load_font(size: int) -> Any:
    try:
        return ImageFont.load_default(size=size)
    except TypeError:  # Pillow < 10.1
        return ImageFont.load_default()


class DryRunBackend:
    """No input is injected. Used with --dry-run and always on non-Windows."""

    dry_run = True

    def __init__(self, abort_check: Callable[[], bool] = lambda: False) -> None:
        self._abort = abort_check
        self._pos = (DRY_RUN_SIZE[0] // 2, DRY_RUN_SIZE[1] // 2)
        self._title = "AVNI dry-run desktop"

    def screen_info(self) -> ScreenInfo:
        return ScreenInfo(*DRY_RUN_SIZE)

    def cursor_pos(self) -> tuple[int, int]:
        return self._pos

    def foreground_window(self) -> dict[str, Any]:
        w, h = DRY_RUN_SIZE
        return {"title": self._title, "hwnd": 0, "rect": {"left": 0, "top": 0, "right": w, "bottom": h}}

    def screenshot(self) -> Any:
        _need_pillow()
        img = Image.new("RGB", DRY_RUN_SIZE, (128, 128, 128))
        draw = ImageDraw.Draw(img)
        draw.text((40, 40), iso_now(), fill=(0, 0, 0), font=_load_font(40))
        draw.text((40, 100), "AVNI hands (dry run, synthetic frame)", fill=(40, 40, 40), font=_load_font(28))
        cx, cy = self._pos
        draw.line((cx - 12, cy, cx + 12, cy), fill=(255, 0, 0), width=2)
        draw.line((cx, cy - 12, cx, cy + 12), fill=(255, 0, 0), width=2)
        return img

    def mouse_move(self, x: int, y: int) -> None:
        self._pos = (x, y)

    def mouse_button(self, x: int, y: int, button: str, down: bool) -> None:
        self._pos = (x, y)

    def mouse_click(self, x: int, y: int, button: str, count: int) -> None:
        self._pos = (x, y)

    def mouse_drag(self, x1: int, y1: int, x2: int, y2: int, button: str, steps: int, hold_ms: int) -> None:
        self._pos = (x2, y2)

    def mouse_scroll(self, x: Optional[int], y: Optional[int], dx: int, dy: int) -> None:
        if x is not None and y is not None:
            self._pos = (x, y)

    def type_text(self, text: str, delay_ms: int) -> int:
        return len(keymap.text_to_ops(text))

    def key_combo(self, tokens: list[keymap.Token]) -> None:
        return None

    def key_hold(self, tokens: list[keymap.Token], duration_ms: int) -> None:
        return None


# ---- Windows / user32 (only defined on win32) -------------------------------
if IS_WINDOWS:
    import ctypes
    from ctypes import wintypes

    ULONG_PTR = ctypes.c_size_t

    class MOUSEINPUT(ctypes.Structure):
        _fields_ = (("dx", wintypes.LONG), ("dy", wintypes.LONG), ("mouseData", wintypes.DWORD),
                    ("dwFlags", wintypes.DWORD), ("time", wintypes.DWORD), ("dwExtraInfo", ULONG_PTR))

    class KEYBDINPUT(ctypes.Structure):
        _fields_ = (("wVk", wintypes.WORD), ("wScan", wintypes.WORD), ("dwFlags", wintypes.DWORD),
                    ("time", wintypes.DWORD), ("dwExtraInfo", ULONG_PTR))

    class HARDWAREINPUT(ctypes.Structure):
        _fields_ = (("uMsg", wintypes.DWORD), ("wParamL", wintypes.WORD), ("wParamH", wintypes.WORD))

    class _INPUT_UNION(ctypes.Union):
        _fields_ = (("mi", MOUSEINPUT), ("ki", KEYBDINPUT), ("hi", HARDWAREINPUT))

    class INPUT(ctypes.Structure):
        _anonymous_ = ("u",)
        _fields_ = (("type", wintypes.DWORD), ("u", _INPUT_UNION))

    INPUT_MOUSE, INPUT_KEYBOARD = 0, 1
    MOUSEEVENTF_MOVE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_VIRTUALDESK = 0x0001, 0x8000, 0x4000
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_HWHEEL = 0x0800, 0x1000
    BUTTON_FLAGS = {  # button -> (down, up)
        "left": (0x0002, 0x0004),
        "right": (0x0008, 0x0010),
        "middle": (0x0020, 0x0040),
    }
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE = 0x0001, 0x0002, 0x0004
    WHEEL_DELTA = 120
    SM_CXSCREEN, SM_CYSCREEN = 0, 1
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN = 76, 77, 78, 79

    user32 = ctypes.WinDLL("user32", use_last_error=True)
    user32.SendInput.argtypes = (wintypes.UINT, ctypes.POINTER(INPUT), ctypes.c_int)
    user32.SendInput.restype = wintypes.UINT
    user32.GetCursorPos.argtypes = (ctypes.POINTER(wintypes.POINT),)
    user32.GetCursorPos.restype = wintypes.BOOL
    user32.SetCursorPos.argtypes = (ctypes.c_int, ctypes.c_int)
    user32.SetCursorPos.restype = wintypes.BOOL
    user32.GetSystemMetrics.argtypes = (ctypes.c_int,)
    user32.GetSystemMetrics.restype = ctypes.c_int
    user32.GetForegroundWindow.argtypes = ()
    user32.GetForegroundWindow.restype = wintypes.HWND
    user32.GetWindowTextLengthW.argtypes = (wintypes.HWND,)
    user32.GetWindowTextLengthW.restype = ctypes.c_int
    user32.GetWindowTextW.argtypes = (wintypes.HWND, wintypes.LPWSTR, ctypes.c_int)
    user32.GetWindowTextW.restype = ctypes.c_int
    user32.GetWindowRect.argtypes = (wintypes.HWND, ctypes.POINTER(wintypes.RECT))
    user32.GetWindowRect.restype = wintypes.BOOL
    user32.VkKeyScanW.argtypes = (wintypes.WCHAR,)
    user32.VkKeyScanW.restype = ctypes.c_short
    user32.MapVirtualKeyW.argtypes = (wintypes.UINT, wintypes.UINT)
    user32.MapVirtualKeyW.restype = wintypes.UINT

    def enable_dpi_awareness() -> str:
        """Make coordinates equal real screenshot pixels. Must run before any metrics call."""
        try:
            user32.SetProcessDpiAwarenessContext.argtypes = (ctypes.c_void_p,)
            user32.SetProcessDpiAwarenessContext.restype = wintypes.BOOL
            if user32.SetProcessDpiAwarenessContext(ctypes.c_void_p(-4)):  # PER_MONITOR_AWARE_V2
                return "per-monitor-v2"
        except (AttributeError, OSError):
            pass
        try:
            if ctypes.WinDLL("shcore").SetProcessDpiAwareness(2) == 0:  # PROCESS_PER_MONITOR_DPI_AWARE
                return "per-monitor"
        except (AttributeError, OSError):
            pass
        try:
            if user32.SetProcessDPIAware():
                return "system"
        except (AttributeError, OSError):
            pass
        return "unaware"

    def oem_encoding() -> str:
        try:
            return f"cp{ctypes.windll.kernel32.GetOEMCP()}"
        except (AttributeError, OSError):
            return "utf-8"

    class Win32Backend:
        """Real input via SendInput. All input is serialised by one lock."""

        dry_run = False
        KEY_GAP_S = 0.008
        COMBO_HOLD_MS = 20

        def __init__(self, abort_check: Callable[[], bool] = lambda: False) -> None:
            self._abort = abort_check
            self._lock = threading.RLock()
            self.dpi_mode = enable_dpi_awareness()

        # ---- queries --------------------------------------------------------
        def screen_info(self) -> ScreenInfo:
            gm = user32.GetSystemMetrics
            return ScreenInfo(gm(SM_CXSCREEN), gm(SM_CYSCREEN), gm(SM_XVIRTUALSCREEN), gm(SM_YVIRTUALSCREEN),
                              gm(SM_CXVIRTUALSCREEN), gm(SM_CYVIRTUALSCREEN))

        def cursor_pos(self) -> tuple[int, int]:
            pt = wintypes.POINT()
            if not user32.GetCursorPos(ctypes.byref(pt)):
                raise OSError(f"GetCursorPos failed (error {ctypes.get_last_error()})")
            return pt.x, pt.y

        def foreground_window(self) -> dict[str, Any]:
            hwnd = user32.GetForegroundWindow()
            rect = wintypes.RECT()
            title = ""
            if hwnd:
                length = user32.GetWindowTextLengthW(hwnd)
                buf = ctypes.create_unicode_buffer(length + 1)
                user32.GetWindowTextW(hwnd, buf, length + 1)
                title = buf.value
                user32.GetWindowRect(hwnd, ctypes.byref(rect))
            return {"title": title, "hwnd": int(hwnd or 0),
                    "rect": {"left": rect.left, "top": rect.top, "right": rect.right, "bottom": rect.bottom}}

        def screenshot(self) -> Any:
            _need_pillow()
            from PIL import ImageGrab

            return ImageGrab.grab(all_screens=False).convert("RGB")

        # ---- low level ------------------------------------------------------
        def _check_abort(self) -> None:
            if self._abort():
                raise ActionAborted()

        def _sleep_ms(self, ms: float) -> None:
            """Sleep in short slices so a PAUSE file interrupts long waits."""
            end = time.monotonic() + ms / 1000.0
            while True:
                self._check_abort()
                left = end - time.monotonic()
                if left <= 0:
                    return
                time.sleep(min(left, 0.05))

        @staticmethod
        def _send(events: list[Any]) -> None:
            arr = (INPUT * len(events))(*events)
            sent = user32.SendInput(len(events), arr, ctypes.sizeof(INPUT))
            if sent != len(events):
                raise OSError(f"SendInput injected {sent}/{len(events)} events (win32 error {ctypes.get_last_error()}); "
                              "the desktop may be locked, or the focused window is elevated (UIPI)")

        @staticmethod
        def _mouse_event(flags: int, dx: int = 0, dy: int = 0, data: int = 0) -> Any:
            ev = INPUT()
            ev.type = INPUT_MOUSE
            ev.mi = MOUSEINPUT(dx, dy, data & 0xFFFFFFFF, flags, 0, 0)
            return ev

        @staticmethod
        def _key_event(vk: int, scan: int, flags: int) -> Any:
            ev = INPUT()
            ev.type = INPUT_KEYBOARD
            ev.ki = KEYBDINPUT(vk, scan, flags, 0, 0)
            return ev

        def _vk_event(self, vk: int, extended: bool, up: bool) -> Any:
            flags = (KEYEVENTF_EXTENDEDKEY if extended else 0) | (KEYEVENTF_KEYUP if up else 0)
            return self._key_event(vk, user32.MapVirtualKeyW(vk, 0), flags)

        # ---- mouse ----------------------------------------------------------
        def _move(self, x: int, y: int, verify: bool = True) -> None:
            nx, ny = normalize_abs(x, y, self.screen_info())
            self._send([self._mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny)])
            if verify:
                time.sleep(0.005)
                if self.cursor_pos() != (x, y):  # rounding of the 0..65535 mapping: nudge to the exact pixel
                    user32.SetCursorPos(x, y)

        def _button(self, button: str, down: bool) -> None:
            flags = BUTTON_FLAGS[button][0 if down else 1]
            self._send([self._mouse_event(flags)])

        def mouse_move(self, x: int, y: int) -> None:
            with self._lock:
                self._move(x, y)

        def mouse_button(self, x: int, y: int, button: str, down: bool) -> None:
            with self._lock:
                self._move(x, y)
                self._button(button, down)

        def mouse_click(self, x: int, y: int, button: str, count: int) -> None:
            with self._lock:
                self._move(x, y)
                for i in range(count):
                    self._button(button, True)
                    time.sleep(0.01)
                    self._button(button, False)
                    if i < count - 1:
                        time.sleep(0.05)  # well inside the double-click interval

        def mouse_drag(self, x1: int, y1: int, x2: int, y2: int, button: str, steps: int, hold_ms: int) -> None:
            with self._lock:
                self._move(x1, y1)
                self._button(button, True)
                try:
                    if hold_ms:
                        self._sleep_ms(hold_ms)
                    for i in range(1, steps + 1):
                        self._check_abort()
                        self._move(x1 + round((x2 - x1) * i / steps), y1 + round((y2 - y1) * i / steps),
                                   verify=(i == steps))
                        time.sleep(0.005)
                finally:
                    self._button(button, False)  # never leave the button stuck down

        def mouse_scroll(self, x: Optional[int], y: Optional[int], dx: int, dy: int) -> None:
            with self._lock:
                if x is not None and y is not None:
                    self._move(x, y)
                if dy:
                    self._send([self._mouse_event(MOUSEEVENTF_WHEEL, data=dy * WHEEL_DELTA)])
                if dx:
                    self._send([self._mouse_event(MOUSEEVENTF_HWHEEL, data=dx * WHEEL_DELTA)])

        # ---- keyboard -------------------------------------------------------
        def type_text(self, text: str, delay_ms: int) -> int:
            ops = keymap.text_to_ops(text)
            with self._lock:
                for op in ops:
                    self._check_abort()
                    if op.vk is not None:
                        self._send([self._vk_event(op.vk, False, False), self._vk_event(op.vk, False, True)])
                    else:
                        events = []
                        for unit in op.units:
                            events.append(self._key_event(0, unit, KEYEVENTF_UNICODE))
                            events.append(self._key_event(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP))
                        self._send(events)
                    if delay_ms:
                        time.sleep(delay_ms / 1000.0)
            return len(ops)

        def _resolve(self, tokens: list[keymap.Token]) -> list[tuple[int, bool]]:
            """Tokens -> [(vk, extended)]; char tokens go through VkKeyScanW (adds implied Shift/Ctrl/Alt)."""
            out: list[tuple[int, bool]] = []
            for tok in tokens:
                if tok.kind != "char":
                    out.append((int(tok.vk or 0), tok.extended))
                    continue
                res = user32.VkKeyScanW(tok.char)
                if res == -1:
                    raise keymap.KeymapError(f"no key produces {tok.char!r} on the current keyboard layout", tok.char or "")
                vk, state = res & 0xFF, (res >> 8) & 0xFF
                for bit, mod_vk in ((1, keymap.VK_SHIFT), (2, keymap.VK_CONTROL), (4, keymap.VK_MENU)):
                    if state & bit and all(v != mod_vk for v, _ in out):
                        out.append((mod_vk, False))
                out.append((vk, False))
            return out

        def _press(self, keys: list[tuple[int, bool]], hold_ms: int) -> None:
            pressed: list[tuple[int, bool]] = []
            try:
                for vk, ext in keys:
                    self._check_abort()
                    self._send([self._vk_event(vk, ext, False)])
                    pressed.append((vk, ext))
                    time.sleep(self.KEY_GAP_S)
                self._sleep_ms(hold_ms)
            finally:
                for vk, ext in reversed(pressed):  # always release, in reverse order
                    try:
                        self._send([self._vk_event(vk, ext, True)])
                    except OSError:
                        pass
                    time.sleep(self.KEY_GAP_S)

        def key_combo(self, tokens: list[keymap.Token]) -> None:
            keys = self._resolve(tokens)
            with self._lock:
                self._press(keys, self.COMBO_HOLD_MS)

        def key_hold(self, tokens: list[keymap.Token], duration_ms: int) -> None:
            keys = self._resolve(tokens)
            with self._lock:
                self._press(keys, duration_ms)
else:  # pragma: no cover - keeps names defined for static readers on Linux/macOS
    Win32Backend = None  # type: ignore[assignment,misc]

    def oem_encoding() -> str:
        return "utf-8"


# =============================================================================
# Shell, screenshots
# =============================================================================
def _kill_tree(proc: "subprocess.Popen[bytes]") -> None:
    try:
        if IS_WINDOWS:
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True, timeout=10,
                           stdin=subprocess.DEVNULL, creationflags=0x08000000)
        else:
            import signal

            os.killpg(proc.pid, signal.SIGKILL)
    except (OSError, subprocess.SubprocessError):
        proc.kill()


def run_shell(cmd: str, timeout_s: int, cwd: Optional[str]) -> dict[str, Any]:
    """Run cmd with shell=True in the guest; output capped at 200 KB per stream."""
    started = time.perf_counter()
    kwargs: dict[str, Any] = {}
    if IS_WINDOWS:
        kwargs["creationflags"] = 0x08000000  # CREATE_NO_WINDOW
    else:
        kwargs["start_new_session"] = True
    proc = subprocess.Popen(cmd, shell=True, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, **kwargs)
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout_s)
    except subprocess.TimeoutExpired:
        timed_out = True
        _kill_tree(proc)
        try:
            out, err = proc.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            out, err = b"", b""
    enc = oem_encoding()
    stdout, cut_out = cap_output(out or b"", SHELL_OUTPUT_CAP, enc)
    stderr, cut_err = cap_output(err or b"", SHELL_OUTPUT_CAP, enc)
    return {"code": -1 if timed_out else proc.returncode, "stdout": stdout, "stderr": stderr,
            "duration_ms": int((time.perf_counter() - started) * 1000),
            "truncated": cut_out or cut_err, "timed_out": timed_out}


def archive_png(img: Any, directory: Path) -> Path:
    """Save the full-resolution frame as <UTC timestamp>.png (learning dataset)."""
    directory.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S_%fZ")
    path = directory / f"{stamp}.png"
    n = 1
    while path.exists():
        path = directory / f"{stamp}_{n}.png"
        n += 1
    img.save(path, "PNG", compress_level=1)
    return path


# =============================================================================
# HTTP layer
# =============================================================================
class HandsServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = not IS_WINDOWS  # on Windows SO_REUSEADDR would allow port hijacking

    def __init__(self, config: ServerConfig, backend: Backend) -> None:
        self.config = config
        self.backend = backend
        self.limiter = RateLimiter(MAX_INPUT_PER_SEC)
        self.action_log = ActionLog(config.log_path)
        super().__init__((config.host, config.port), Handler)

    def handle_error(self, request: Any, client_address: Any) -> None:
        if isinstance(sys.exc_info()[1], (ConnectionError, TimeoutError)):
            return
        self.action_log.write({"ts": iso_now(), "endpoint": "(server)", "error": "unhandled",
                               "traceback": traceback.format_exc()})


@dataclass
class Ctx:
    server: HandsServer
    method: str
    path: str
    query: dict[str, list[str]]
    body: dict[str, Any] = field(default_factory=dict)
    note: dict[str, Any] = field(default_factory=dict)

    @property
    def backend(self) -> Backend:
        return self.server.backend

    @property
    def config(self) -> ServerConfig:
        return self.server.config

    def q(self, key: str, default: str) -> str:
        vals = self.query.get(key)
        return vals[0] if vals else default


# ---- endpoints --------------------------------------------------------------
def ep_health(ctx: Ctx) -> dict[str, Any]:
    screen = ctx.backend.screen_info()
    out = {"ok": True, "service": SERVICE, "version": VERSION, "hostname": socket.gethostname(),
           "screen": {"width": screen.width, "height": screen.height}, "paused": ctx.config.is_paused(),
           "time": iso_now()}
    if ctx.backend.dry_run:
        out["dry_run"] = True
    return out


def ep_screenshot(ctx: Ctx) -> Response:
    scale = parse_scale(ctx.q("scale", "1.0"))
    if ctx.q("format", "png").lower() != "png":
        raise ApiError(400, "only format=png is supported", field="format")
    mode = ctx.q("as", "").lower()
    if mode not in ("", "png", "raw", "json"):
        raise ApiError(400, "as must be 'json' or omitted", field="as")
    img = ctx.backend.screenshot()
    full_w, full_h = img.size
    if ctx.config.archive_dir is not None:
        try:
            ctx.note["archived"] = str(archive_png(img, ctx.config.archive_dir))
        except OSError as exc:
            ctx.note["archive_error"] = str(exc)
    out = img
    if scale < 1.0:
        out = img.resize((max(1, round(full_w * scale)), max(1, round(full_h * scale))), Image.Resampling.LANCZOS)
    png = encode_png(out)
    headers = {"X-Screen-Width": str(full_w), "X-Screen-Height": str(full_h),
               "X-Image-Width": str(out.width), "X-Image-Height": str(out.height), "Cache-Control": "no-store"}
    if mode == "json":
        return Response(200, payload={"width": out.width, "height": out.height, "screen_width": full_w,
                                      "screen_height": full_h, "png_base64": base64.b64encode(png).decode("ascii")},
                        headers=headers)
    return Response(200, body=png, content_type="image/png", headers=headers)


def ep_cursor(ctx: Ctx) -> dict[str, Any]:
    x, y = ctx.backend.cursor_pos()
    return {"x": x, "y": y}


def ep_window(ctx: Ctx) -> dict[str, Any]:
    return ctx.backend.foreground_window()


def ep_uia(ctx: Ctx) -> dict[str, Any]:
    # TODO(v2): UI Automation tree via pywinauto/comtypes (element names, roles, bounding boxes).
    raise ApiError(501, "not implemented in v1",
                   detail="UI Automation is planned for a later version; use /screenshot and coordinates for now")


def ep_mouse_move(ctx: Ctx) -> dict[str, Any]:
    x, y = validate_xy(ctx.body, ctx.backend.screen_info())
    ctx.backend.mouse_move(x, y)
    return {"x": x, "y": y}


def ep_mouse_click(ctx: Ctx) -> dict[str, Any]:
    x, y = validate_xy(ctx.body, ctx.backend.screen_info())
    button, count = validate_button(ctx.body), validate_count(ctx.body)
    ctx.backend.mouse_click(x, y, button, count)
    return {"x": x, "y": y, "button": button, "count": count}


def _button_endpoint(ctx: Ctx, down: bool) -> dict[str, Any]:
    x, y = validate_xy(ctx.body, ctx.backend.screen_info())
    button = validate_button(ctx.body)
    ctx.backend.mouse_button(x, y, button, down)
    return {"x": x, "y": y, "button": button}


def ep_mouse_down(ctx: Ctx) -> dict[str, Any]:
    return _button_endpoint(ctx, True)


def ep_mouse_up(ctx: Ctx) -> dict[str, Any]:
    return _button_endpoint(ctx, False)


def ep_mouse_drag(ctx: Ctx) -> dict[str, Any]:
    screen = ctx.backend.screen_info()
    x1, y1 = validate_point(ctx.body, "from", screen)
    x2, y2 = validate_point(ctx.body, "to", screen)
    button = validate_button(ctx.body)
    steps = get_int(ctx.body, "steps", 1, 500, default=20)
    hold_ms = get_int(ctx.body, "hold_ms", 0, MAX_WAIT_MS, default=0)
    ctx.backend.mouse_drag(x1, y1, x2, y2, button, steps, hold_ms)
    return {"from": {"x": x1, "y": y1}, "to": {"x": x2, "y": y2}, "button": button, "steps": steps}


def ep_mouse_scroll(ctx: Ctx) -> dict[str, Any]:
    screen = ctx.backend.screen_info()
    x = y = None
    if "x" in ctx.body or "y" in ctx.body:  # position optional: omitted = scroll at the current cursor
        x, y = validate_xy(ctx.body, screen)
    dx = get_int(ctx.body, "dx", -100, 100, default=0)
    dy = get_int(ctx.body, "dy", -100, 100, default=0)
    if dx == 0 and dy == 0:
        raise ApiError(400, "dx and dy are both zero; nothing to scroll (positive dy = up, negative = down)")
    ctx.backend.mouse_scroll(x, y, dx, dy)
    return {"x": x, "y": y, "dx": dx, "dy": dy}


def ep_type(ctx: Ctx) -> dict[str, Any]:
    text = ctx.body.get("text")
    if not isinstance(text, str):
        raise ApiError(400, "text must be a string", field="text")
    if len(text) > MAX_TEXT_CHARS:
        raise ApiError(400, f"text is longer than {MAX_TEXT_CHARS} characters", field="text")
    delay_ms = get_int(ctx.body, "delay_ms", 0, 1000, default=10)
    typed = ctx.backend.type_text(text, delay_ms)
    return {"chars": typed, "delay_ms": delay_ms}


def _combo(ctx: Ctx, multi: bool) -> list[keymap.Token]:
    combo = ctx.body.get("combo")
    if not isinstance(combo, str):
        raise ApiError(400, "combo must be a string such as 'ctrl+shift+s'", field="combo")
    return keymap.parse_combo(combo, allow_multiple_keys=multi)


def ep_key(ctx: Ctx) -> dict[str, Any]:
    tokens = _combo(ctx, multi=False)
    ctx.backend.key_combo(tokens)
    return {"combo": ctx.body["combo"], "keys": [t.name for t in tokens]}


def ep_hold(ctx: Ctx) -> dict[str, Any]:
    tokens = _combo(ctx, multi=True)
    ms = get_int(ctx.body, "duration_ms", 0, 10**9)
    held = min(ms, MAX_WAIT_MS)
    ctx.backend.key_hold(tokens, held)
    return {"combo": ctx.body["combo"], "keys": [t.name for t in tokens], "held_ms": held, "clamped": held != ms}


def ep_wait(ctx: Ctx) -> dict[str, Any]:
    ms = get_int(ctx.body, "ms", 0, 10**9)
    waited = min(ms, MAX_WAIT_MS)
    time.sleep(waited / 1000.0)
    return {"waited_ms": waited, "clamped": waited != ms}


def ep_shell(ctx: Ctx) -> dict[str, Any]:
    cmd = ctx.body.get("cmd")
    if not isinstance(cmd, str) or not cmd.strip():
        raise ApiError(400, "cmd must be a non-empty string", field="cmd")
    timeout_s = get_int(ctx.body, "timeout_s", 1, 600, default=60)
    cwd = ctx.body.get("cwd")
    if cwd is not None and (not isinstance(cwd, str) or not os.path.isdir(cwd)):
        raise ApiError(400, "cwd must be null or an existing directory", field="cwd")
    if ctx.backend.dry_run:  # never execute arbitrary commands on a dev box
        return {"code": 0, "stdout": "", "stderr": "", "duration_ms": 0, "truncated": False, "timed_out": False}
    result = run_shell(cmd, timeout_s, cwd)
    ctx.note["code"] = result["code"]
    return result


@dataclass(frozen=True)
class Route:
    fn: Callable[[Ctx], Any]
    kind: str  # "read" (no wrap) | "input" (pause + rate guard + wrap) | "wait" (wrap only)


ROUTES: dict[tuple[str, str], Route] = {
    ("GET", "/health"): Route(ep_health, "read"),
    ("GET", "/screenshot"): Route(ep_screenshot, "read"),
    ("GET", "/cursor"): Route(ep_cursor, "read"),
    ("GET", "/window"): Route(ep_window, "read"),
    ("GET", "/uia"): Route(ep_uia, "read"),
    ("POST", "/mouse/move"): Route(ep_mouse_move, "input"),
    ("POST", "/mouse/click"): Route(ep_mouse_click, "input"),
    ("POST", "/mouse/down"): Route(ep_mouse_down, "input"),
    ("POST", "/mouse/up"): Route(ep_mouse_up, "input"),
    ("POST", "/mouse/drag"): Route(ep_mouse_drag, "input"),
    ("POST", "/mouse/scroll"): Route(ep_mouse_scroll, "input"),
    ("POST", "/keyboard/type"): Route(ep_type, "input"),
    ("POST", "/keyboard/key"): Route(ep_key, "input"),
    ("POST", "/keyboard/hold"): Route(ep_hold, "input"),
    ("POST", "/wait"): Route(ep_wait, "wait"),
    ("POST", "/shell"): Route(ep_shell, "input"),
}


class Handler(BaseHTTPRequestHandler):
    server: HandsServer  # type: ignore[assignment]
    protocol_version = "HTTP/1.1"
    server_version = f"{SERVICE}/{VERSION}"
    timeout = 120

    # -- plumbing ---------------------------------------------------------------
    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - silence stdlib stderr logging
        return None

    def send_error(self, code: int, message: Optional[str] = None, explain: Optional[str] = None) -> None:
        self.close_connection = True
        self._send(ApiError(code, message or "bad request").to_response())

    def _send(self, resp: Response) -> None:
        data = resp.body if resp.body is not None else json.dumps(resp.payload or {}, ensure_ascii=False).encode("utf-8")
        try:
            self.send_response(resp.status)
            self.send_header("Content-Type", resp.content_type)
            self.send_header("Content-Length", str(len(data)))
            for key, value in resp.headers.items():
                self.send_header(key, value)
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)
        except (ConnectionError, TimeoutError):
            self.close_connection = True

    def _read_body(self) -> bytes:
        if self.headers.get("Transfer-Encoding"):
            self.close_connection = True
            raise ApiError(400, "chunked request bodies are not supported; send Content-Length")
        header = self.headers.get("Content-Length")
        if not header:
            return b""
        try:
            length = int(header)
        except ValueError:
            self.close_connection = True
            raise ApiError(400, "invalid Content-Length") from None
        if length < 0 or length > MAX_BODY_BYTES:
            self.close_connection = True
            raise ApiError(413, f"request body larger than {MAX_BODY_BYTES} bytes")
        return self.rfile.read(length)

    # -- dispatch -----------------------------------------------------------------
    def _handle(self) -> None:
        started = time.perf_counter()
        method = self.command
        parts = urlsplit(self.path)
        path = parts.path.rstrip("/") or "/"
        record: dict[str, Any] = {"ts": iso_now(), "client": self.client_address[0], "method": method, "endpoint": path}
        ctx: Optional[Ctx] = None
        try:
            raw = self._read_body()
            if not token_matches(self.server.config.token, self.headers.get("X-Avni-Token")):
                raise ApiError(401, "missing or invalid X-Avni-Token")
            route = ROUTES.get((method, path))
            if route is None:
                raise ApiError(404, f"no such endpoint: {method} {path}")
            ctx = Ctx(self.server, method, path, parse_qs(parts.query))
            resp = self._execute(route, ctx, raw)
        except ApiError as exc:
            resp = exc.to_response()
        except keymap.KeymapError as exc:
            resp = ApiError(400, str(exc), token=exc.token).to_response()
        except ActionAborted:
            resp = ApiError(423, "paused", aborted=True).to_response()
        except Exception as exc:  # noqa: BLE001 - never leak a traceback to the client
            record["traceback"] = traceback.format_exc()
            resp = ApiError(500, f"{type(exc).__name__}: {exc}").to_response()
        record["status"] = resp.status
        record["duration_ms"] = int((time.perf_counter() - started) * 1000)
        if ctx is not None:
            if ctx.body:
                record["body"] = trim_for_log(ctx.body)
            if ctx.note:
                record["note"] = ctx.note
        if resp.payload is not None:
            record["result"] = trim_for_log({k: v for k, v in resp.payload.items() if k != "png_base64"})
        else:
            record["result"] = {"bytes": len(resp.body or b""), "content_type": resp.content_type}
        if self.server.config.dry_run:
            record["dry_run"] = True
        self.server.action_log.write(record)  # logged before replying: a client that saw the reply can rely on the line
        self._send(resp)

    def _execute(self, route: Route, ctx: Ctx, raw: bytes) -> Response:
        if route.kind == "input" and ctx.config.is_paused():
            raise ApiError(423, "paused")
        if route.kind in ("input", "wait"):
            ctx.body = parse_json_body(raw)
        if route.kind == "input" and not self.server.limiter.allow():
            raise ApiError(429, f"rate limit: max {MAX_INPUT_PER_SEC} input actions per second",
                           headers={"Retry-After": "1"})
        t0 = time.perf_counter()
        result = route.fn(ctx)
        if isinstance(result, Response):
            return result
        if route.kind == "read":
            return Response(200, payload=result)
        payload = {"ok": True, "action": ctx.path.strip("/").replace("/", "_"),
                   "took_ms": int((time.perf_counter() - t0) * 1000)}
        payload.update(result)
        if ctx.backend.dry_run:
            payload["dry_run"] = True
        return Response(200, payload=payload)

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = do_HEAD = do_OPTIONS = _handle


# =============================================================================
# Startup
# =============================================================================
def say(message: str) -> None:
    try:
        print(message, flush=True)  # under pythonw stdout is None; print is then a no-op
    except (OSError, ValueError):
        pass


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="avni_hands", description="AVNI guest hands service (screen, mouse, keyboard)")
    p.add_argument("--host", default="0.0.0.0")
    p.add_argument("--port", type=int, default=DEFAULT_PORT, help="0 = pick a free port")
    p.add_argument("--token-file", default=None, help=r"default: %%LOCALAPPDATA%%\Avni\workbench\hands.token")
    p.add_argument("--archive-dir", default=None, help="also save every /screenshot here as <UTC timestamp>.png")
    p.add_argument("--dry-run", action="store_true", help="log input instead of injecting it; synthetic screenshots")
    return p


def main(argv: Optional[list[str]] = None) -> int:
    args = build_parser().parse_args(argv)
    base = default_base_dir()
    log = ActionLog(base / "hands.log")
    dry_run = bool(args.dry_run) or not IS_WINDOWS
    if dry_run and not args.dry_run:
        say("note: not running on Windows, so --dry-run is forced")
    if not dry_run and not os.environ.get("AVNI_HANDS_ALLOW_HOST"):
        is_guest, detail = looks_like_hyperv_guest()
        if not is_guest:
            msg = (f"REFUSING TO START: this does not look like a Hyper-V guest ({detail}). Sending input on the "
                   "host would hijack the owner's real mouse and keyboard. Run it inside the VM, or set "
                   "AVNI_HANDS_ALLOW_HOST=1 if you really mean it.")
            say(msg)
            log.write({"ts": iso_now(), "endpoint": "(startup)", "error": msg})
            return 2
    if not 0 <= args.port <= 65535:
        say("error: --port must be between 0 and 65535")
        return 2

    token_path = Path(args.token_file) if args.token_file else base / "hands.token"
    token, created = load_or_create_token(token_path)
    config = ServerConfig(host=args.host, port=args.port, token=token, dry_run=dry_run, token_path=token_path,
                          log_path=base / "hands.log", pause_path=base / "PAUSE",
                          archive_dir=Path(args.archive_dir) if args.archive_dir else None)
    backend: Backend = DryRunBackend(config.is_paused) if dry_run else Win32Backend(config.is_paused)  # type: ignore[misc]
    try:
        server = HandsServer(config, backend)
    except OSError as exc:
        say(f"error: cannot listen on {args.host}:{args.port}: {exc}")
        return 1
    port = server.server_address[1]
    say(f"{SERVICE} {VERSION} listening on {args.host}:{port} ({'DRY-RUN' if dry_run else 'LIVE input'})")
    say(f"token file : {token_path}" + ("  (newly generated)" if created else ""))
    say(f"log file   : {config.log_path}")
    say(f"pause file : {config.pause_path}  (create it to pause all input)")
    log.write({"ts": iso_now(), "endpoint": "(startup)", "version": VERSION, "port": port, "dry_run": dry_run,
               "host": socket.gethostname(), "dpi": getattr(backend, "dpi_mode", None),
               "token_file": str(token_path), "archive_dir": str(config.archive_dir) if config.archive_dir else None})
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
