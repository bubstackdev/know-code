"""Tests for check_mermaid.py: how a golden file is split into its diagrams."""
import unittest

import check_mermaid


class SplitTest(unittest.TestCase):
    def test_each_title_with_its_diagram(self):
        text = "=== flow a\nflowchart TD\n  n0[\"a\"]\n\n=== path a -> b\nflowchart LR\n"
        self.assertEqual(check_mermaid.split(text), [("flow a", "flowchart TD\n  n0[\"a\"]\n\n"), ("path a -> b", "flowchart LR\n")])


if __name__ == "__main__":
    unittest.main()
