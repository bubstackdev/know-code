"""schema_cost.py's requests, offline: what each case puts in front of the model. No network."""
import unittest

import schema_cost

TOOLS = [
    {"name": "kc_context", "description": "One symbol.", "inputSchema": {"type": "object"}, "_meta": {"anthropic/alwaysLoad": True}},
    {"name": "kc_find", "description": "By name.", "inputSchema": {"type": "object"}},
]


class Requests(unittest.TestCase):
    def test_tools_are_sent_as_a_client_names_them(self):
        self.assertEqual(schema_cost.api_tool(TOOLS[1]), {"name": "mcp__knowcode__kc_find", "description": "By name.", "input_schema": {"type": "object"}})
        self.assertEqual([schema_cost.always_loaded(t) for t in TOOLS], [True, False])

    def test_the_base_differs_from_a_case_only_by_knowcodes_part(self):
        base = schema_cost.request("m")
        case = schema_cost.request("m", [schema_cost.api_tool(TOOLS[0])], "Query it.", schema_cost.deferred_listing(["kc_find"]))
        self.assertEqual(case["tools"][0], base["tools"][0])
        self.assertEqual(case["tools"][1]["name"], "mcp__knowcode__kc_context")
        self.assertTrue(case["system"].startswith(base["system"]) and case["system"].endswith("Query it."))
        content = case["messages"][0]["content"]
        self.assertTrue(content.endswith(base["messages"][0]["content"]))
        self.assertIn("\nmcp__knowcode__kc_find\n", content)


if __name__ == "__main__":
    unittest.main()
