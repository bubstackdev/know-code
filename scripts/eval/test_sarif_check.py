"""Tests for sarif_check.py: logs that quote source are caught."""
import unittest

import sarif_check


class SourceTextTest(unittest.TestCase):
    def test_a_snippet_or_context_region_is_source_text(self):
        region = {"startLine": 3}
        self.assertFalse(sarif_check.source_text({"runs": [{"results": [{"locations": [{"physicalLocation": {"region": region}}]}]}]}))
        self.assertTrue(sarif_check.source_text({"region": {"snippet": {"text": "x"}}}))
        self.assertTrue(sarif_check.source_text({"physicalLocation": {"contextRegion": region}}))


if __name__ == "__main__":
    unittest.main()
