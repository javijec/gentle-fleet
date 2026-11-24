import unittest
from local_ci import overall, plan, validate_report, expected_ids, manual_rows


class LocalCITests(unittest.TestCase):
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
        self.assertIn("e2e_p62b_desktop_gpui", expected_ids("linux", "full"))

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
