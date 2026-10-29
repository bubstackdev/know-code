"""Tests for rules_gate.py: how ast-grep's JSON is read and how two match sets are compared."""
import json
import unittest

import rules_gate


def sg(rule, file, start, end, severity="warning", message="m"):
    return json.dumps({
        "ruleId": rule,
        "file": file,
        "severity": severity,
        "message": message,
        "range": {"byteOffset": {"start": start, "end": end}, "start": {"line": 0, "column": 0}},
    })


class ParseTest(unittest.TestCase):
    def test_stream_lines_become_matches_without_a_leading_dot(self):
        text = sg("r", "./src/a.cs", 3, 9) + "\n\n" + sg("q", "b.ts", 0, 1, "error", "x") + "\n"
        self.assertEqual(rules_gate.parse_stream(text), [
            {"rule": "r", "file": "src/a.cs", "start": 3, "end": 9, "severity": "warning", "message": "m"},
            {"rule": "q", "file": "b.ts", "start": 0, "end": 1, "severity": "error", "message": "x"},
        ])


class CompareTest(unittest.TestCase):
    def m(self, rule, start, message="m"):
        return {"rule": rule, "file": "a.cs", "start": start, "end": start + 1, "severity": "hint", "message": message}

    def test_equal_sets_have_no_difference(self):
        a = [self.m("r", 1), self.m("r", 5)]
        self.assertEqual(rules_gate.compare(a, list(reversed(a))), ([], [], []))

    def test_matches_are_keyed_by_rule_file_and_bytes(self):
        missing, extra, differ = rules_gate.compare([self.m("r", 1), self.m("q", 1)], [self.m("r", 1), self.m("r", 2)])
        self.assertEqual(missing, [("q", "a.cs", 1, 2)])
        self.assertEqual(extra, [("r", "a.cs", 2, 3)])
        self.assertEqual(differ, [])

    def test_a_message_difference_is_reported_apart(self):
        self.assertEqual(rules_gate.compare([self.m("r", 1, "a")], [self.m("r", 1, "b")]), ([], [], [("r", "a.cs", 1, 2)]))


if __name__ == "__main__":
    unittest.main()
