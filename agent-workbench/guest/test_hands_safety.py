"""Tests for the review-round-1 safety additions to avni_hands.py:
client allow-list, screenshot archive retention, shell opt-in and PAUSE abort.
Runs on any OS (no user32)."""
from __future__ import annotations

import json
import os
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import avni_hands as hands  # noqa: E402


class ClientAllowList(unittest.TestCase):
    def test_empty_set_means_any(self) -> None:
        self.assertTrue(hands.client_allowed("10.9.8.7", frozenset()))

    def test_exact_match(self) -> None:
        allowed = frozenset({"127.0.0.1", "172.26.32.1"})
        self.assertTrue(hands.client_allowed("172.26.32.1", allowed))
        self.assertFalse(hands.client_allowed("172.26.32.55", allowed))

    def test_ipv4_mapped_ipv6(self) -> None:
        self.assertTrue(hands.client_allowed("::ffff:127.0.0.1", frozenset({"127.0.0.1"})))

    def test_default_gateway_shape(self) -> None:
        gw = hands.default_gateway_ipv4()
        if gw is not None:
            self.assertRegex(gw, r"^\d{1,3}(\.\d{1,3}){3}$")


class ArchiveRetention(unittest.TestCase):
    def test_trims_oldest_only_own_files(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            names = ["20261010T010000_000000Z.png", "20261010T010001_000000Z.png",
                     "20261010T010002_000000Z.png", "20261010T010002_000000Z_1.png"]
            for n in names:
                (root / n).write_bytes(b"x" * 1000)
            (root / "keep-me.png").write_bytes(b"x" * 5000)  # not ours: must survive
            (root / "notes.txt").write_text("hello")
            removed = hands.trim_archive(root, 2500)
            self.assertEqual([p.name for p in removed], names[:2])
            self.assertTrue((root / "keep-me.png").exists())
            self.assertTrue((root / "notes.txt").exists())
            self.assertTrue((root / names[2]).exists())

    def test_zero_limit_disables(self) -> None:
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "20261010T010000_000000Z.png").write_bytes(b"x" * 10)
            self.assertEqual(hands.trim_archive(Path(d), 0), [])
            self.assertTrue((Path(d) / "20261010T010000_000000Z.png").exists())


@unittest.skipIf(hands.IS_WINDOWS, "uses POSIX sleep/echo")
class ShellAbort(unittest.TestCase):
    def test_completes_and_captures(self) -> None:
        r = hands.run_shell("echo out; echo err 1>&2; exit 3", 10, None)
        self.assertEqual(r["code"], 3)
        self.assertEqual(r["stdout"].strip(), "out")
        self.assertEqual(r["stderr"].strip(), "err")
        self.assertFalse(r["aborted"])
        self.assertFalse(r["timed_out"])

    def test_pause_aborts_running_command(self) -> None:
        flag = {"pause": False}
        threading.Timer(0.4, lambda: flag.__setitem__("pause", True)).start()
        t0 = time.monotonic()
        r = hands.run_shell("sleep 20", 30, None, abort_check=lambda: flag["pause"], poll_s=0.1)
        self.assertTrue(r["aborted"])
        self.assertEqual(r["code"], -2)
        self.assertLess(time.monotonic() - t0, 5.0)

    def test_timeout_kills(self) -> None:
        r = hands.run_shell("sleep 20", 0.5, None, poll_s=0.1)
        self.assertTrue(r["timed_out"])
        self.assertEqual(r["code"], -1)

    def test_big_output_capped_not_deadlocked(self) -> None:
        r = hands.run_shell("head -c 600000 /dev/zero | tr '\\0' 'a'", 10, None)
        self.assertTrue(r["truncated"])
        self.assertEqual(len(r["stdout"]), hands.SHELL_OUTPUT_CAP)


class ServerPolicy(unittest.TestCase):
    """Boot the real server in dry-run on an ephemeral port and check the 403 paths."""

    def _serve(self, allowed: frozenset[str], shell: bool) -> tuple[hands.HandsServer, str, Path]:
        d = Path(tempfile.mkdtemp())
        cfg = hands.ServerConfig(host="127.0.0.1", port=0, token="t0k", dry_run=True, token_path=d / "tok",
                                 log_path=d / "hands.log", pause_path=d / "PAUSE", archive_dir=None,
                                 allowed_clients=allowed, shell_enabled=shell)
        srv = hands.HandsServer(cfg, hands.DryRunBackend(cfg.is_paused))
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        return srv, f"http://127.0.0.1:{srv.server_address[1]}", d

    @staticmethod
    def _call(url: str, method: str = "GET", body: dict | None = None, token: str | None = "t0k") -> tuple[int, dict]:
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(url, data=data, method=method)
        if token:
            req.add_header("X-Avni-Token", token)
        if data:
            req.add_header("Content-Type", "application/json")
        try:
            with urllib.request.urlopen(req, timeout=5) as r:
                return r.status, json.loads(r.read() or b"{}")
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read() or b"{}")

    def test_disallowed_client_gets_403_before_auth(self) -> None:
        srv, base, _ = self._serve(frozenset({"10.0.0.1"}), shell=False)
        try:
            status, body = self._call(base + "/health")
            self.assertEqual(status, 403)
            self.assertIn("not allowed", body["error"])
        finally:
            srv.shutdown()

    def test_allowed_client_and_shell_disabled(self) -> None:
        srv, base, _ = self._serve(frozenset({"127.0.0.1"}), shell=False)
        try:
            self.assertEqual(self._call(base + "/health")[0], 200)
            status, body = self._call(base + "/shell", "POST", {"cmd": "echo hi"})
            self.assertEqual(status, 403)
            self.assertIn("--enable-shell", body["error"])
        finally:
            srv.shutdown()

    def test_shell_enabled_dry_run(self) -> None:
        srv, base, _ = self._serve(frozenset(), shell=True)
        try:
            status, body = self._call(base + "/shell", "POST", {"cmd": "echo hi"})
            self.assertEqual(status, 200)
            self.assertTrue(body["dry_run"])
            self.assertFalse(body["aborted"])
        finally:
            srv.shutdown()


if __name__ == "__main__":
    unittest.main()
