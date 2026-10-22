"""Key-name translation for the AVNI guest "hands" service.

Pure Python, no Windows imports, so it can be unit-tested anywhere.

``parse_combo("ctrl+shift+S")`` returns a list of :class:`Token` objects,
modifiers first (in the fixed order ctrl, alt, shift, super) and then the key.

Token kinds:
  * ``"modifier"`` - ctrl / alt / shift / super. ``vk`` is the generic Windows
    virtual-key code (VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN).
  * ``"vk"``       - a named key (Return, Tab, F5, KP_3, ...). ``vk`` is set.
  * ``"char"``     - a single character such as ``a``, ``7`` or ``;``. The
    layout-dependent virtual key (and any implied Shift) is resolved by the
    service at runtime with VkKeyScanW, so ``vk`` is None and ``char`` is set.

``text_to_ops`` turns free text into per-character typing operations
(UTF-16 code units for KEYEVENTF_UNICODE, or a real Enter/Tab key press).
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Final, Optional

# --- Windows virtual-key codes (winuser.h) ---------------------------------
VK_BACK: Final = 0x08
VK_TAB: Final = 0x09
VK_RETURN: Final = 0x0D
VK_SHIFT: Final = 0x10
VK_CONTROL: Final = 0x11
VK_MENU: Final = 0x12  # Alt
VK_PAUSE: Final = 0x13
VK_CAPITAL: Final = 0x14
VK_ESCAPE: Final = 0x1B
VK_SPACE: Final = 0x20
VK_PRIOR: Final = 0x21  # Page Up
VK_NEXT: Final = 0x22  # Page Down
VK_END: Final = 0x23
VK_HOME: Final = 0x24
VK_LEFT: Final = 0x25
VK_UP: Final = 0x26
VK_RIGHT: Final = 0x27
VK_DOWN: Final = 0x28
VK_SNAPSHOT: Final = 0x2C  # Print Screen
VK_INSERT: Final = 0x2D
VK_DELETE: Final = 0x2E
VK_LWIN: Final = 0x5B
VK_APPS: Final = 0x5D  # context-menu key
VK_NUMPAD0: Final = 0x60
VK_MULTIPLY: Final = 0x6A
VK_ADD: Final = 0x6B
VK_SUBTRACT: Final = 0x6D
VK_DECIMAL: Final = 0x6E
VK_DIVIDE: Final = 0x6F
VK_F1: Final = 0x70
VK_NUMLOCK: Final = 0x90
VK_SCROLL: Final = 0x91


class KeymapError(ValueError):
    """Raised for an unknown or malformed key name; ``token`` is the offender."""

    def __init__(self, message: str, token: str = "") -> None:
        super().__init__(message)
        self.token = token


@dataclass(frozen=True)
class Token:
    name: str  # canonical lower-case name, e.g. "ctrl", "enter", "a"
    kind: str  # "modifier" | "vk" | "char"
    vk: Optional[int] = None
    char: Optional[str] = None
    extended: bool = False  # needs KEYEVENTF_EXTENDEDKEY


# --- modifiers ---------------------------------------------------------------
MODIFIER_ORDER: Final = ("ctrl", "alt", "shift", "super")
MODIFIER_VK: Final = {"ctrl": VK_CONTROL, "alt": VK_MENU, "shift": VK_SHIFT, "super": VK_LWIN}
MODIFIER_ALIASES: Final = {
    "ctrl": "ctrl", "control": "ctrl",
    "alt": "alt",
    "shift": "shift",
    "super": "super", "win": "super", "windows": "super", "meta": "super",
    "cmd": "super", "command": "super",
}

# --- named keys: canonical-or-alias name -> (vk, extended) -------------------
NAMED_KEYS: dict[str, tuple[int, bool]] = {}


def _add(vk: int, extended: bool, *names: str) -> None:
    for name in names:
        NAMED_KEYS[name] = (vk, extended)


def _build_named_keys() -> None:
    _add(VK_RETURN, False, "return", "enter")
    _add(VK_TAB, False, "tab")
    _add(VK_ESCAPE, False, "escape", "esc")
    _add(VK_BACK, False, "backspace")
    _add(VK_DELETE, True, "delete", "del")
    _add(VK_HOME, True, "home")
    _add(VK_END, True, "end")
    _add(VK_PRIOR, True, "page_up", "pageup", "prior", "pgup")
    _add(VK_NEXT, True, "page_down", "pagedown", "next", "pgdn")
    _add(VK_UP, True, "up")
    _add(VK_DOWN, True, "down")
    _add(VK_LEFT, True, "left")
    _add(VK_RIGHT, True, "right")
    _add(VK_SPACE, False, "space")
    _add(VK_INSERT, True, "insert", "ins")
    _add(VK_SNAPSHOT, True, "print", "printscreen", "prtsc", "sysrq")
    _add(VK_APPS, True, "menu", "apps")
    _add(VK_CAPITAL, False, "caps_lock", "capslock", "caps")
    _add(VK_NUMLOCK, True, "num_lock", "numlock")
    _add(VK_SCROLL, False, "scroll_lock", "scrolllock")
    _add(VK_PAUSE, False, "pause", "break")
    for n in range(1, 25):
        _add(VK_F1 + n - 1, False, f"f{n}")
    for n in range(10):
        _add(VK_NUMPAD0 + n, False, f"kp_{n}", f"kp{n}", f"numpad{n}")
    _add(VK_ADD, False, "kp_add")
    _add(VK_SUBTRACT, False, "kp_subtract")
    _add(VK_MULTIPLY, False, "kp_multiply")
    _add(VK_DIVIDE, True, "kp_divide")
    _add(VK_DECIMAL, False, "kp_decimal")
    _add(VK_RETURN, True, "kp_enter")


_build_named_keys()

# xdotool/X11 keysym names for punctuation -> the character they produce.
CHAR_NAMES: Final = {
    "plus": "+", "minus": "-", "equal": "=", "underscore": "_",
    "comma": ",", "period": ".", "slash": "/", "backslash": "\\",
    "semicolon": ";", "colon": ":", "apostrophe": "'", "quotedbl": '"',
    "grave": "`", "asciitilde": "~", "exclam": "!", "at": "@",
    "numbersign": "#", "dollar": "$", "percent": "%", "asciicircum": "^",
    "ampersand": "&", "asterisk": "*", "parenleft": "(", "parenright": ")",
    "bracketleft": "[", "bracketright": "]", "braceleft": "{", "braceright": "}",
    "bar": "|", "less": "<", "greater": ">", "question": "?",
}


def _split_combo(combo: str) -> list[str]:
    """Split on '+', keeping a trailing literal '+' ("ctrl++" -> ctrl, +)."""
    s = combo.strip()
    if not s:
        raise KeymapError("empty key combination", "")
    if s == "+":
        return ["+"]
    if s.endswith("++"):
        return s[:-2].split("+") + ["+"]
    return s.split("+")


def _parse_part(part: str, combo: str) -> Token:
    raw = part.strip()
    if not raw:
        raise KeymapError(f"empty token in key combination {combo!r}", part)
    low = raw.lower()
    if low in MODIFIER_ALIASES:
        name = MODIFIER_ALIASES[low]
        return Token(name, "modifier", vk=MODIFIER_VK[name], extended=(name == "super"))
    if len(raw) == 1:
        if not raw.isprintable() or raw.isspace():
            raise KeymapError(f"unknown key name: {raw!r}", raw)
        return Token(low, "char", char=low)
    key = low.replace("-", "_").replace(" ", "_")
    if key in NAMED_KEYS:
        vk, ext = NAMED_KEYS[key]
        return Token(key, "vk", vk=vk, extended=ext)
    if key in CHAR_NAMES:
        ch = CHAR_NAMES[key]
        return Token(key, "char", char=ch)
    raise KeymapError(f"unknown key name: {raw!r}", raw)


def parse_combo(combo: str, *, allow_multiple_keys: bool = False) -> list[Token]:
    """Parse an xdotool-style combo into tokens: modifiers first, then key(s).

    Case-insensitive. Duplicate modifiers collapse. Modifiers are emitted in the
    fixed order ctrl, alt, shift, super regardless of how they were written.
    Only one non-modifier key is allowed unless ``allow_multiple_keys`` (used by
    /keyboard/hold). Raises :class:`KeymapError` with the offending token.
    """
    if not isinstance(combo, str):
        raise KeymapError("combo must be a string", str(combo))
    tokens = [_parse_part(p, combo) for p in _split_combo(combo)]
    mods = {t.name: t for t in tokens if t.kind == "modifier"}
    keys = [t for t in tokens if t.kind != "modifier"]
    if len(keys) > 1 and not allow_multiple_keys:
        raise KeymapError(
            f"more than one non-modifier key in {combo!r}: {keys[0].name!r}, {keys[1].name!r}",
            keys[1].name,
        )
    ordered = [mods[m] for m in MODIFIER_ORDER if m in mods]
    return ordered + keys


# --- free-text typing --------------------------------------------------------
@dataclass(frozen=True)
class TextOp:
    """One typed character: either a real key press (Enter/Tab) or UTF-16 units."""

    vk: Optional[int] = None
    units: tuple[int, ...] = ()


def text_to_ops(text: str) -> list[TextOp]:
    """Translate text into typing operations, one per character.

    CRLF / CR / LF become one Enter key press, TAB becomes a Tab key press
    (apps ignore Unicode-injected control characters). Every other character is
    sent as KEYEVENTF_UNICODE UTF-16 code units, so Devanagari (Hindi/Marathi)
    and astral characters (surrogate pairs) work.
    """
    ops: list[TextOp] = []
    i, n = 0, len(text)
    while i < n:
        ch = text[i]
        if ch == "\r":
            ops.append(TextOp(vk=VK_RETURN))
            i += 2 if text[i + 1:i + 2] == "\n" else 1
            continue
        if ch == "\n":
            ops.append(TextOp(vk=VK_RETURN))
        elif ch == "\t":
            ops.append(TextOp(vk=VK_TAB))
        else:
            raw = ch.encode("utf-16-le", "surrogatepass")
            ops.append(TextOp(units=tuple(int.from_bytes(raw[k:k + 2], "little") for k in range(0, len(raw), 2))))
        i += 1
    return ops
