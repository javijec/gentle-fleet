import base64
import unittest
from unittest.mock import patch

from both import remote_command, validate_native


class BothTests(unittest.TestCase):
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
