import unittest
from contextlib import redirect_stdout
from io import StringIO
from unittest.mock import patch

from console import result_line, show_result, summary
from local_ci import overall, plan, validate_report, expected_ids, manual_rows


class LocalCITests(unittest.TestCase):
    def test_console_colors_and_success_log_hidden(self):
        row = dict(id="build", status="PASS", log="private.log", seconds=1.2)
        self.assertIn("\x1b[32mPASS\x1b[0m", result_line(row, enabled=True))
        self.assertIn("\x1b[31mFAIL\x1b[0m", result_line(dict(row, status="FAIL"), enabled=True))
        with patch("console.color_enabled", return_value=False), redirect_stdout(StringIO()) as output:
            show_result(row)
            summary("TOTAL", "PASS", [row], "private.json")
            self.assertNotIn("private", output.getvalue())
            show_result(dict(row, status="FAIL"))
            summary("TOTAL", "FAIL", report="private.json")
            self.assertIn("private.log", output.getvalue())
            self.assertIn("private.json", output.getvalue())

    def test_status(self):
        self.assertEqual(overall([{"status": "PASS"}]), "PASS")
        self.assertEqual(overall([{"status": "SKIP"}]), "BLOCKED")
        self.assertEqual(overall([{"status": "FAIL"}, {"status": "BLOCKED"}]), "FAIL")
        self.assertEqual(overall([]), "BLOCKED")

    def test_plans(self):
        for os_name in ("windows", "linux"):
            quick = {x["id"] for x in plan(os_name, "quick")}
            full = {x["id"] for x in plan(os_name, "full")}
            self.assertTrue(quick < full)
            self.assertIn("build", full)
        self.assertIn("w1b-named-pipe", expected_ids("windows", "full"))
        self.assertIn("e2e_p44_security", expected_ids("linux", "full"))
        self.assertIn("e2e_p61_sdk", expected_ids("linux", "full"))
        self.assertNotIn("e2e_p62b_desktop_gpui", expected_ids("linux", "full"))
        for os_name in ("windows", "linux"):
            self.assertIn("headless-graph", expected_ids(os_name, "full"))
            self.assertIn("manual-external-sdk-smoke", expected_ids(os_name, "release"))
            self.assertNotIn("manual-desktop-smoke", expected_ids(os_name, "release"))
        self.assertIn("manual-cross-user-ipc-acl", expected_ids("windows", "release"))
        self.assertIn("manual-ipc-backpressure-cancel-peer-identity", expected_ids("windows", "release"))

    def test_report_integrity(self):
        data = dict(schema=1, source="local", platform="windows", commit="a",
                    profile="full", finished="now", status="PASS",
                    checks=[dict(id=x, status="PASS") for x in expected_ids("windows", "full")])
        self.assertEqual(validate_report(data, "windows", "a", "full"), "PASS")
        with self.assertRaises(ValueError):
            validate_report(data, "windows", "b", "full")
        data["checks"].append(dict(data["checks"][0]))
        with self.assertRaises(ValueError):
            validate_report(data, "windows", "a", "full")
        data["checks"].pop()
        data["checks"][0]["status"] = "SKIP"
        with self.assertRaises(ValueError):
            validate_report(data, "windows", "a", "full")
        data["checks"][0]["status"] = "PASS"
        data["checks"].pop()
        with self.assertRaises(ValueError):
            validate_report(data, "windows", "a", "full")

    def test_release_without_manual_evidence(self):
        self.assertEqual(overall(manual_rows("windows", "a", None)), "BLOCKED")


if __name__ == "__main__":
    unittest.main()
