import base64
import json
from argparse import Namespace
from contextlib import redirect_stdout
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from both import coordinator, prepare_format, remote_command, rust_diff, sync_command, validate_native


class BothTests(unittest.TestCase):
    def test_coordinator_final_summary_and_exit_code(self):
        # Exercise the real finalization path, not only report validation.
        args = Namespace(mode="w1c", windows="local", linux="fixture",
                         windows_root=None, linux_root="/tmp/fixture")
        sha = "a" * 40
        for fail in (False, True):
            with self.subTest(fail=fail), TemporaryDirectory() as directory:
                root = Path(directory)

                def invoke(_host, _root, platform, arguments, _sha, capture=False):
                    status = "FAIL" if fail and platform == "linux" else "PASS"
                    action = arguments[arguments.index("--worker") + 1]
                    if action == "collect":
                        record = {"status": status, "ci": {"checks": [{"status": status}]},
                                  "protocol_runs": []}
                        return SimpleNamespace(returncode=0, stdout=json.dumps(record))
                    return SimpleNamespace(returncode=int(action == "run" and status == "FAIL"))

                with patch("both.ROOT", root), patch("both.clean_head", return_value=sha), \
                     patch("both.current_platform", return_value="windows"), \
                     patch("both.invoke", side_effect=invoke), \
                     patch("both.validate_native", side_effect=lambda record, *_: record["status"]), \
                     patch("console.color_enabled", return_value=False), redirect_stdout(StringIO()) as output:
                    code = coordinator(args)
                expected = "FAIL" if fail else "AUTOMATED_PASS"
                self.assertEqual(code, int(fail))
                self.assertIn("TOTAL: " + expected, output.getvalue())
                self.assertEqual("Report:" in output.getvalue(), fail)
                report, = root.glob("reports/combined/*/combined.json")
                data = json.loads(report.read_text(encoding="utf-8"))
                self.assertEqual(data["status"], expected)
                self.assertEqual(data["commit"], sha)
                self.assertTrue(data["finished"])

    def test_auto_format_rejects_unexpected_paths(self):
        for status in (b"?? stray.rs\0", b"M  staged.rs\0", b" M Cargo.toml\0"):
            with self.subTest(status=status), patch("both.subprocess.check_output", return_value=status):
                with self.assertRaises(RuntimeError):
                    rust_diff()
        with patch("both.subprocess.check_output", side_effect=[b" M crates/a b.rs\0", b"patch"]):
            self.assertEqual(rust_diff(), b"patch")

    def test_clean_format_does_not_commit_or_push(self):
        sha = "a" * 40
        with patch("both.clean_head", return_value=sha), \
             patch("both.git", side_effect=["feature/p7", sha]), \
             patch("both.git_run") as git_run, \
             patch("both.subprocess.run") as cargo, \
             patch("both.rust_diff", return_value=b""), \
             redirect_stdout(StringIO()):
            self.assertEqual(prepare_format(), ("feature/p7", sha))
        git_run.assert_called_once_with("fetch", "--no-tags", "origin", "refs/heads/feature/p7")
        self.assertEqual(cargo.call_args.args[0], ["cargo", "fmt", "--all"])

    def test_format_declined_never_stages_or_pushes(self):
        sha = "a" * 40
        with patch("both.clean_head", return_value=sha), \
             patch("both.git", side_effect=["feature/p7", sha]), \
             patch("both.git_run") as git_run, \
             patch("both.subprocess.run"), \
             patch("both.rust_diff", return_value=b"patch"), \
             patch("builtins.input", return_value="n"), redirect_stdout(StringIO()):
            with self.assertRaisesRegex(RuntimeError, "left uncommitted"):
                prepare_format()
        self.assertEqual([call.args[0] for call in git_run.call_args_list], ["fetch", "--no-pager"])

    def test_sync_command_preserves_checkout_guards(self):
        command = sync_command("/tmp/a folder", "feature/p7", "a" * 40)
        encoded = command.split("b64decode('", 1)[1].split("')", 1)[0]
        source = base64.b64decode(encoded).decode("utf-8")
        compile(source, "<remote-sync>", "exec")
        self.assertIn("remote checkout dirty", source)
        self.assertIn("refs/heads/feature/p7", source)
        self.assertIn("merge-base", source)
        self.assertIn("--no-overwrite-ignore", source)
        self.assertIn("--detach", source)
        self.assertNotIn("--force", source)
        self.assertNotIn("reset", source)

    def test_remote_shell_quoting(self):
        root = "C:\\a folder\\repo & (test)"
        command = remote_command(root, ["--worker", "preflight"], "a" * 40)
        self.assertNotIn(root, command)
        encoded = command.split("b64decode('", 1)[1].split("')", 1)[0]
        source = base64.b64decode(encoded).decode("utf-8")
        compile(source, "<remote>", "exec")
        self.assertIn(repr(root), source)
        self.assertIn(repr("a" * 40), source)

    @patch("both.validate_report", return_value="PASS")
    def test_w1c_requires_ten_windows_runs(self, _validate):
        data = dict(platform="windows", commit="a", mode="w1c", status="PASS",
                    finished="now", ci_exit_code=0, ci={}, protocol_runs=[])
        with self.assertRaises(ValueError):
            validate_native(data, "windows", "a", "w1c")
        data["protocol_runs"] = [dict(id=f"protocol-{i:02d}", status="PASS", exit_code=0)
                                 for i in range(1, 11)]
        self.assertEqual(validate_native(data, "windows", "a", "w1c"), "PASS")
        data["protocol_runs"][0]["status"] = "SKIP"
        with self.assertRaises(ValueError):
            validate_native(data, "windows", "a", "w1c")

    @patch("both.validate_report", return_value="PASS")
    def test_w1f_requires_twenty_lifecycle_runs_and_ten_protocol_runs(self, _validate):
        data = dict(platform="windows", commit="a", mode="w1f", status="PASS",
                    finished="now", ci_exit_code=0, ci={},
                    protocol_runs=[dict(id=f"protocol-{i:02d}", status="PASS", exit_code=0)
                                   for i in range(1, 11)])
        with self.assertRaisesRegex(ValueError, "lifecycle"):
            validate_native(data, "windows", "a", "w1f")
        data["lifecycle_runs"] = [dict(id=f"lifecycle-{i:02d}", status="PASS", exit_code=0)
                                  for i in range(1, 21)]
        self.assertEqual(validate_native(data, "windows", "a", "w1f"), "PASS")
        data["lifecycle_runs"][-1]["exit_code"] = 1
        with self.assertRaisesRegex(ValueError, "lifecycle"):
            validate_native(data, "windows", "a", "w1f")
        data["lifecycle_runs"][-1]["exit_code"] = 0
        data["protocol_runs"].pop()
        with self.assertRaisesRegex(ValueError, "protocol"):
            validate_native(data, "windows", "a", "w1f")
        linux = dict(platform="linux", commit="a", mode="w1f", status="PASS",
                     finished="now", ci_exit_code=0, ci={}, protocol_runs=[], lifecycle_runs=[])
        self.assertEqual(validate_native(linux, "linux", "a", "w1f"), "PASS")
        linux["lifecycle_runs"] = data["lifecycle_runs"]
        with self.assertRaisesRegex(ValueError, "lifecycle"):
            validate_native(linux, "linux", "a", "w1f")

    @patch("both.validate_report", return_value="PASS")
    def test_reject_other_sha_or_platform(self, _validate):
        data = dict(platform="linux", commit="a", mode="full", status="PASS",
                    finished="now", ci_exit_code=0, ci={}, protocol_runs=[])
        self.assertEqual(validate_native(data, "linux", "a", "full"), "PASS")
        with self.assertRaises(ValueError):
            validate_native(data, "linux", "b", "full")
        with self.assertRaises(ValueError):
            validate_native(data, "windows", "a", "full")


if __name__ == "__main__":
    unittest.main()
