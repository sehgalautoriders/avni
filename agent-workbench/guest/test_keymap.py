"""Unit tests for keymap.py, the request validators and the dry-run HTTP server.

Run from this folder:  python3 -m unittest -v
Everything here runs on any OS (no user32 is touched).
"""
from __future__ import annotations

import json
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

import avni_hands as hands
import keymap
from keymap import KeymapError, parse_combo, text_to_ops

SCREEN = hands.ScreenInfo(1280, 720)


def names(combo: str, **kw) -> list[str]:
    return [t.name for t in parse_combo(combo, **kw)]


class ParseComboTests(unittest.TestCase):
    def test_simple_combo(self) -> None:
        toks = parse_combo("ctrl+shift+s")
        self.assertEqual([t.name for t in toks], ["ctrl", "shift", "s"])
        self.assertEqual([t.kind for t in toks], ["modifier", "modifier", "char"])
        self.assertEqual(toks[-1].char, "s")
        self.assertIsNone(toks[-1].vk)  # chars are resolved by VkKeyScanW at runtime

    def test_modifier_aliases(self) -> None:
        for alias in ("control", "Ctrl", "CONTROL"):
            self.assertEqual(names(f"{alias}+a"), ["ctrl", "a"])
        for alias in ("super", "win", "meta", "cmd", "Windows"):
            self.assertEqual(names(f"{alias}+d"), ["super", "d"])
        self.assertEqual(parse_combo("win+e")[0].vk, keymap.VK_LWIN)
        self.assertEqual(parse_combo("alt+f4")[0].vk, keymap.VK_MENU)
        self.assertEqual(parse_combo("ctrl+c")[0].vk, keymap.VK_CONTROL)
        self.assertEqual(parse_combo("shift+a")[0].vk, keymap.VK_SHIFT)

    def test_modifier_ordering_is_canonical_and_first(self) -> None:
        self.assertEqual(names("shift+alt+ctrl+Delete"), ["ctrl", "alt", "shift", "delete"])
        self.assertEqual(names("a+ctrl"), ["ctrl", "a"])  # modifiers move in front of the key
        self.assertEqual(names("super+shift+s"), ["shift", "super", "s"])

    def test_duplicate_modifiers_collapse(self) -> None:
        self.assertEqual(names("ctrl+control+a"), ["ctrl", "a"])

    def test_named_keys_and_aliases(self) -> None:
        cases = {
            "Return": keymap.VK_RETURN, "enter": keymap.VK_RETURN, "Tab": keymap.VK_TAB,
            "Escape": keymap.VK_ESCAPE, "esc": keymap.VK_ESCAPE, "BackSpace": keymap.VK_BACK,
            "backspace": keymap.VK_BACK, "Delete": keymap.VK_DELETE, "Home": keymap.VK_HOME,
            "End": keymap.VK_END, "Page_Up": keymap.VK_PRIOR, "pageup": keymap.VK_PRIOR,
            "Page_Down": keymap.VK_NEXT, "PageDown": keymap.VK_NEXT, "Up": keymap.VK_UP,
            "Down": keymap.VK_DOWN, "Left": keymap.VK_LEFT, "Right": keymap.VK_RIGHT,
            "space": keymap.VK_SPACE, "Insert": keymap.VK_INSERT, "Print": keymap.VK_SNAPSHOT,
            "Menu": keymap.VK_APPS, "apps": keymap.VK_APPS, "Caps_Lock": keymap.VK_CAPITAL,
            "Num_Lock": keymap.VK_NUMLOCK,
        }
        for name, vk in cases.items():
            tok = parse_combo(name)[0]
            self.assertEqual((tok.kind, tok.vk), ("vk", vk), name)

    def test_function_keys_f1_to_f24(self) -> None:
        self.assertEqual(parse_combo("F1")[0].vk, 0x70)
        self.assertEqual(parse_combo("f12")[0].vk, 0x7B)
        self.assertEqual(parse_combo("F24")[0].vk, 0x87)
        with self.assertRaises(KeymapError):
            parse_combo("F25")

    def test_numpad(self) -> None:
        self.assertEqual(parse_combo("KP_0")[0].vk, 0x60)
        self.assertEqual(parse_combo("kp_9")[0].vk, 0x69)

    def test_case_insensitive_chars_and_digits(self) -> None:
        self.assertEqual(parse_combo("CTRL+A")[1].char, "a")
        tok = parse_combo("7")[0]
        self.assertEqual((tok.kind, tok.char), ("char", "7"))

    def test_punctuation_and_keysym_names(self) -> None:
        self.assertEqual(parse_combo(";")[0].char, ";")
        self.assertEqual(parse_combo("ctrl+plus")[1].char, "+")
        self.assertEqual(parse_combo("ctrl++")[1].char, "+")  # literal plus at the end
        self.assertEqual(parse_combo("comma")[0].char, ",")
        self.assertEqual(parse_combo("bracketleft")[0].char, "[")

    def test_extended_flag_on_navigation_keys(self) -> None:
        self.assertTrue(parse_combo("Left")[0].extended)
        self.assertTrue(parse_combo("Delete")[0].extended)
        self.assertFalse(parse_combo("Return")[0].extended)

    def test_modifier_only_combo(self) -> None:
        self.assertEqual(names("shift"), ["shift"])
        self.assertEqual(names("ctrl+alt"), ["ctrl", "alt"])

    def test_unknown_token_reports_offender(self) -> None:
        with self.assertRaises(KeymapError) as cm:
            parse_combo("ctrl+Frobnicate")
        self.assertEqual(cm.exception.token, "Frobnicate")
        self.assertIn("Frobnicate", str(cm.exception))

    def test_empty_and_malformed(self) -> None:
        for bad in ("", "   ", "ctrl+", "+ctrl", "ctrl++a"):
            with self.assertRaises(KeymapError, msg=repr(bad)):
                parse_combo(bad)

    def test_two_keys_rejected_unless_hold(self) -> None:
        with self.assertRaises(KeymapError):
            parse_combo("a+b")
        self.assertEqual(names("w+d", allow_multiple_keys=True), ["w", "d"])

    def test_vk_table_matches_windows_constants(self) -> None:
        self.assertEqual(keymap.VK_RETURN, 0x0D)
        self.assertEqual(keymap.VK_SNAPSHOT, 0x2C)
        self.assertEqual(keymap.VK_CAPITAL, 0x14)
        self.assertEqual(keymap.VK_NUMLOCK, 0x90)


class TextOpsTests(unittest.TestCase):
    def test_ascii_and_newline(self) -> None:
        ops = text_to_ops("a\r\nb\n\tc")
        self.assertEqual([o.vk for o in ops], [None, keymap.VK_RETURN, None, keymap.VK_RETURN, keymap.VK_TAB, None])
        self.assertEqual(ops[0].units, (ord("a"),))

    def test_devanagari_is_bmp_units(self) -> None:
        ops = text_to_ops("मराठी")  # Marathi
        self.assertEqual(len(ops), 5)
        self.assertEqual(ops[0].units, (0x092E,))

    def test_astral_char_is_surrogate_pair(self) -> None:
        ops = text_to_ops("\U0001F600")
        self.assertEqual(len(ops), 1)
        self.assertEqual(ops[0].units, (0xD83D, 0xDE00))


class ValidatorTests(unittest.TestCase):
    def assert400(self, fn, *args) -> hands.ApiError:
        with self.assertRaises(hands.ApiError) as cm:
            fn(*args)
        self.assertEqual(cm.exception.status, 400)
        return cm.exception

    def test_coordinates_inside_bounds(self) -> None:
        self.assertEqual(hands.validate_xy({"x": 0, "y": 0}, SCREEN), (0, 0))
        self.assertEqual(hands.validate_xy({"x": 1279, "y": 719}, SCREEN), (1279, 719))
        self.assertEqual(hands.validate_xy({"x": 10.0, "y": 20.0}, SCREEN), (10, 20))

    def test_coordinates_out_of_bounds(self) -> None:
        self.assert400(hands.validate_xy, {"x": 1280, "y": 0}, SCREEN)
        self.assert400(hands.validate_xy, {"x": 0, "y": 720}, SCREEN)
        self.assert400(hands.validate_xy, {"x": -1, "y": 5}, SCREEN)

    def test_coordinates_must_be_ints(self) -> None:
        for bad in ("10", 10.5, True, None, [1]):
            self.assert400(hands.validate_xy, {"x": bad, "y": 1}, SCREEN)
        err = self.assert400(hands.validate_xy, {"y": 1}, SCREEN)
        self.assertEqual(err.extra["field"], "x")

    def test_button_set(self) -> None:
        self.assertEqual(hands.validate_button({}), "left")
        self.assertEqual(hands.validate_button({"button": "RIGHT"}), "right")
        self.assertEqual(hands.validate_button({"button": "middle"}), "middle")
        self.assert400(hands.validate_button, {"button": "back"})
        self.assert400(hands.validate_button, {"button": 1})

    def test_count_range(self) -> None:
        self.assertEqual(hands.validate_count({}), 1)
        self.assertEqual(hands.validate_count({"count": 3}), 3)
        for bad in (0, 4, -1, "2", 1.5):
            self.assert400(hands.validate_count, {"count": bad})

    def test_drag_point_field_name(self) -> None:
        err = self.assert400(hands.validate_point, {"from": {"x": 5000, "y": 1}}, "from", SCREEN)
        self.assertEqual(err.extra["field"], "from.x")
        self.assert400(hands.validate_point, {}, "to", SCREEN)

    def test_json_body(self) -> None:
        self.assertEqual(hands.parse_json_body(b""), {})
        self.assertEqual(hands.parse_json_body(b'{"a": 1}'), {"a": 1})
        self.assert400(hands.parse_json_body, b"{not json")
        self.assert400(hands.parse_json_body, b"[1, 2]")

    def test_scale(self) -> None:
        self.assertEqual(hands.parse_scale("0.5"), 0.5)
        for bad in ("0.05", "1.5", "abc", "nan"):
            self.assert400(hands.parse_scale, bad)

    def test_normalize_abs(self) -> None:
        self.assertEqual(hands.normalize_abs(0, 0, SCREEN), (0, 0))
        self.assertEqual(hands.normalize_abs(1279, 719, SCREEN), (65535, 65535))
        mid = hands.normalize_abs(640, 360, SCREEN)
        self.assertTrue(32000 < mid[0] < 33500 and 32000 < mid[1] < 33500)


class MiscHelperTests(unittest.TestCase):
    def test_rate_limiter(self) -> None:
        now = [0.0]
        rl = hands.RateLimiter(3, clock=lambda: now[0])
        self.assertEqual([rl.allow() for _ in range(4)], [True, True, True, False])
        now[0] = 1.01
        self.assertTrue(rl.allow())

    def test_token_matches(self) -> None:
        self.assertTrue(hands.token_matches("abc", "abc"))
        self.assertFalse(hands.token_matches("abc", "abd"))
        self.assertFalse(hands.token_matches("abc", None))
        self.assertFalse(hands.token_matches("abc", ""))

    def test_token_file_created_once(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "sub" / "hands.token"
            tok, created = hands.load_or_create_token(path)
            self.assertTrue(created)
            self.assertGreaterEqual(len(tok), 43)  # 32 bytes urlsafe-base64
            self.assertEqual(hands.load_or_create_token(path), (tok, False))

    def test_cap_output(self) -> None:
        text, cut = hands.cap_output(b"x" * 10, 4)
        self.assertEqual((text, cut), ("xxxx", True))
        self.assertEqual(hands.cap_output(b"hi", 4), ("hi", False))

    def test_guest_detection(self) -> None:
        self.assertTrue(hands.model_indicates_hyperv_guest("Virtual Machine"))
        self.assertFalse(hands.model_indicates_hyperv_guest("ThinkPad X1"))
        self.assertFalse(hands.model_indicates_hyperv_guest(None))
        self.assertEqual(hands.looks_like_hyperv_guest([lambda: None, lambda: "Virtual Machine"])[0], True)
        self.assertEqual(hands.looks_like_hyperv_guest([lambda: "OptiPlex 7090"])[0], False)
        self.assertEqual(hands.looks_like_hyperv_guest([lambda: None])[0], False)


class DryRunServerTests(unittest.TestCase):
    TOKEN = "test-token-123"

    @classmethod
    def setUpClass(cls) -> None:
        cls.tmp = tempfile.TemporaryDirectory()
        base = Path(cls.tmp.name)
        cls.config = hands.ServerConfig("127.0.0.1", 0, cls.TOKEN, True, base / "t", base / "hands.log",
                                        base / "PAUSE", base / "shots")
        cls.server = hands.HandsServer(cls.config, hands.DryRunBackend(cls.config.is_paused))
        cls.port = cls.server.server_address[1]
        threading.Thread(target=cls.server.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.server.server_close()
        cls.tmp.cleanup()

    def setUp(self) -> None:
        self.config.pause_path.unlink(missing_ok=True)
        self.server.limiter = hands.RateLimiter(hands.MAX_INPUT_PER_SEC)

    def call(self, method: str, path: str, body=None, token=TOKEN, raw: bytes | None = None):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        req = urllib.request.Request(f"http://127.0.0.1:{self.port}{path}", data=data, method=method)
        if token:
            req.add_header("X-Avni-Token", token)
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return r.status, r.read(), dict(r.headers)
        except urllib.error.HTTPError as e:
            return e.code, e.read(), dict(e.headers)

    def jcall(self, *a, **kw):
        status, body, headers = self.call(*a, **kw)
        return status, json.loads(body), headers

    def test_health_and_auth(self) -> None:
        status, body, _ = self.jcall("GET", "/health")
        self.assertEqual(status, 200)
        self.assertEqual((body["service"], body["version"], body["screen"]), ("avni-hands", hands.VERSION, {"width": 1280, "height": 720}))
        self.assertEqual(self.jcall("GET", "/health", token=None)[0], 401)
        self.assertEqual(self.jcall("GET", "/health", token="wrong")[0], 401)

    def test_screenshot_headers_and_scale(self) -> None:
        status, png, h = self.call("GET", "/screenshot?scale=0.5")
        self.assertEqual(status, 200)
        self.assertTrue(png.startswith(b"\x89PNG"))
        self.assertEqual((h["X-Screen-Width"], h["X-Screen-Height"], h["X-Image-Width"], h["X-Image-Height"]),
                         ("1280", "720", "640", "360"))
        self.assertEqual(self.jcall("GET", "/screenshot?scale=2")[0], 400)
        status, body, _ = self.jcall("GET", "/screenshot?as=json")
        self.assertEqual((body["width"], body["height"]), (1280, 720))
        self.assertTrue(any(self.config.archive_dir.glob("*.png")))

    def test_key_endpoint(self) -> None:
        status, body, _ = self.jcall("POST", "/keyboard/key", {"combo": "ctrl+shift+s"})
        self.assertEqual((status, body["ok"], body["action"], body["dry_run"]), (200, True, "keyboard_key", True))
        status, body, _ = self.jcall("POST", "/keyboard/key", {"combo": "ctrl+Bogus"})
        self.assertEqual((status, body["token"]), (400, "Bogus"))

    def test_mouse_validation_and_cursor(self) -> None:
        self.assertEqual(self.jcall("POST", "/mouse/click", {"x": 100, "y": 200, "count": 2})[0], 200)
        self.assertEqual(self.jcall("GET", "/cursor")[1], {"x": 100, "y": 200})
        self.assertEqual(self.jcall("POST", "/mouse/click", {"x": 5000, "y": 1})[0], 400)
        self.assertEqual(self.jcall("POST", "/mouse/scroll", {"x": 1, "y": 1, "dx": 0, "dy": -3})[0], 200)

    def test_pause_blocks_input_not_reads(self) -> None:
        self.config.pause_path.write_text("")
        status, body, _ = self.jcall("POST", "/mouse/move", {"x": 1, "y": 1})
        self.assertEqual((status, body), (423, {"error": "paused"}))
        self.assertEqual(self.jcall("GET", "/health")[1]["paused"], True)
        self.assertEqual(self.call("GET", "/screenshot")[0], 200)

    def test_errors_are_json(self) -> None:
        self.assertEqual(self.jcall("GET", "/nope")[0], 404)
        self.assertEqual(self.jcall("POST", "/keyboard/key", raw=b"{broken")[0], 400)
        status, body, _ = self.jcall("GET", "/uia")
        self.assertEqual((status, body["error"]), (501, "not implemented in v1"))

    def test_rate_guard(self) -> None:
        codes = [self.jcall("POST", "/mouse/move", {"x": 1, "y": 1})[0] for _ in range(hands.MAX_INPUT_PER_SEC + 5)]
        self.assertIn(429, codes)
        self.assertEqual(codes[0], 200)

    def test_shell_disabled_by_default(self) -> None:
        # Review round 1: /shell is opt-in (--enable-shell). The dry-run execution path is
        # covered in test_hands_safety.ServerPolicy.test_shell_enabled_dry_run.
        status, body, _ = self.jcall("POST", "/shell", {"cmd": "echo hi"})
        self.assertEqual(status, 403)
        self.assertIn("--enable-shell", body["error"])

    def test_log_is_written(self) -> None:
        self.jcall("POST", "/wait", {"ms": 1})
        lines = self.config.log_path.read_text(encoding="utf-8").splitlines()
        rec = json.loads(lines[-1])
        self.assertEqual((rec["endpoint"], rec["status"]), ("/wait", 200))
        self.assertNotIn(self.TOKEN, "\n".join(lines))


if __name__ == "__main__":
    unittest.main()
