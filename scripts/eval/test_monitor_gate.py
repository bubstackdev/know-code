"""Tests for monitor_gate.py: what counts as new, which files both sides read, and how runs are compared."""
import unittest
from unittest import mock

import monitor_gate


def m(rule, text):
    return {"rule": rule, "text": text}


class SurplusTest(unittest.TestCase):
    def test_moved_and_renamed_matches_are_not_new(self):
        self.assertEqual(monitor_gate.surplus([m("r", "a.Result")], [m("r", "a.Result")]), 0)

    def test_one_more_of_a_text_or_a_new_text_counts(self):
        old = [m("r", "a.Result")]
        new = [m("r", "a.Result"), m("r", "a.Result"), m("r", "b.Result"), m("q", "a.Result")]
        self.assertEqual(monitor_gate.surplus(new, old), 3)

    def test_removed_matches_never_count(self):
        self.assertEqual(monitor_gate.surplus([], [m("r", "a.Result")]), 0)


class ChangedTest(unittest.TestCase):
    def raw(self, *records):
        return "".join(records)

    def test_statuses_renames_and_left_out_paths(self):
        z, a, b = "0" * 40, "1" * 40, "2" * 40
        out = (
            f":100644 100644 {a} {b} M\0src/A.cs\0"
            f":000000 100644 {z} {b} A\0src/New.cs\0"
            f":100644 000000 {a} {z} D\0src/Gone.cs\0"
            f":100644 100644 {a} {b} R090\0src/Old.cs\0src/Moved.cs\0"
            f":100644 100644 {a} {b} M\0vendor/lib/X.cs\0"
            f":120000 120000 {a} {b} M\0src/link.cs\0"
        )
        with mock.patch.object(monitor_gate, "git", return_value=out):
            self.assertEqual(monitor_gate.changed("clone", "p", "s"), [
                ("src/A.cs", "src/A.cs"),
                (None, "src/New.cs"),
                ("src/Gone.cs", None),
                ("src/Old.cs", "src/Moved.cs"),
            ])


class TextSurplusTest(unittest.TestCase):
    def test_added_matching_lines_beyond_identical_removed_ones(self):
        patch = (
            "diff --git a/a.ts b/a.ts\n--- a/a.ts\n+++ b/a.ts\n@@ -1 +1 @@\n"
            "-  console.log(1);\n+  console.log(1);\n+  console.log(2);\n--- console.log(3) in a removed line\n"
        )
        self.assertEqual(monitor_gate.text_surplus(patch, r"console\.log"), 1)

    def test_only_removed_or_moved_lines_are_not_new(self):
        patch = "diff --git a/a.ts b/b.ts\n--- a/a.ts\n+++ b/b.ts\n@@ -3 +0,0 @@\n-// TODO x\n@@ -0,0 +9 @@\n+// TODO x\n"
        self.assertEqual(monitor_gate.text_surplus(patch, "TODO"), 0)


class CompareTest(unittest.TestCase):
    def test_only_commits_whose_firing_differs_are_listed(self):
        labels = [{"commit": "a", "fires": True}, {"commit": "b", "fires": False}, {"commit": "c", "fires": False}]
        results = [{"commit": "a", "fires": True}, {"commit": "b", "fires": True}]
        self.assertEqual(monitor_gate.compare(labels, results), [("b", False, True), ("c", False, None)])


if __name__ == "__main__":
    unittest.main()
