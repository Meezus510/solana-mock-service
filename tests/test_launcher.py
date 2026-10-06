"""Shutdown must never signal an unrelated replacement for an expired PID."""

import contextlib
import io
import sys
from pathlib import Path
from types import SimpleNamespace
from unittest import TestCase
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
from simulate import Stack


class OwnedShutdown(TestCase):
    def test_reused_pid_is_forgotten_without_signalling_replacement(self):
        stack = Stack.__new__(Stack)
        stack.state = {
            "processes": {
                "worker": {
                    "pid": 123,
                    "command": ["our-worker"],
                    "started": "old-start",
                }
            }
        }
        stack.save = Mock()
        with (
            patch(
                "simulate.subprocess.run",
                side_effect=[
                    SimpleNamespace(stdout="unrelated-worker"),
                    SimpleNamespace(stdout="new-start"),
                    SimpleNamespace(stdout="S"),
                ],
            ),
            patch("simulate.os.killpg") as kill,
            contextlib.redirect_stdout(io.StringIO()),
        ):
            stack.stop("worker")
        kill.assert_not_called()
        self.assertEqual(stack.state["processes"], {})
        stack.save.assert_called_once()


class ReadinessStatus(TestCase):
    def test_pipeline_unready_fails_even_when_http_is_ready(self):
        stack = Stack.__new__(Stack)
        stack.urls = {"strategy": "http://127.0.0.1:18090"}
        stack.state = {"processes": {}}
        stack.root = Path("/tmp")
        stack.path = Mock()
        stack.path.exists.return_value = True
        with patch("simulate.http", return_value={"ready": True, "pipeline_ready": False}), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "strategy"):
                stack.status()

    def test_transport_failure_fails_status(self):
        stack = Stack.__new__(Stack)
        stack.urls = {"market": "http://127.0.0.1:18088"}
        stack.state = {"processes": {}}
        stack.root = Path("/tmp")
        stack.path = Mock()
        stack.path.exists.return_value = True
        with patch("simulate.http", side_effect=ConnectionError), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, "market"):
                stack.status()
