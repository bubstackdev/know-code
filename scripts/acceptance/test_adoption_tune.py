"""Tests for adoption_tune.py: the stream parser, the spend ledger, the session command and the question set.

python3 -m unittest discover -s scripts/acceptance -p 'test_*.py'
"""
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import adoption_tune as at  # noqa: E402

STUDY = Path(__file__).resolve().parents[2] / "eval" / "public-v1" / "study" / "questions.jsonl"


def tool(tool_name, **inp):
    return {"type": "tool_use", "id": tool_name, "name": tool_name, "input": inp}


class StreamTest(unittest.TestCase):
    def test_counts_knowcode_plain_and_meta_calls_and_cost(self):
        lines = [
            {"type": "system", "subtype": "init", "tools": ["Read", "Bash", "mcp__knowcode__kc_refs"]},
            {"type": "assistant", "message": {"content": [
                tool("mcp__knowcode__kc_refs", name="Order.AddOrderItem"),
                tool("Bash", command="grep -rn AddOrderItem src"),
                tool("Bash", command="knowcode refs AddOrderItem"),
                tool("Skill", skill="knowcode:knowcode-explore"),
                {"type": "text", "text": "Three call sites."}]}},
            {"type": "assistant", "message": {"content": [tool("Read", file_path="src/a.cs")]}},
            {"type": "result", "total_cost_usd": 0.0421, "num_turns": 3, "is_error": False, "result": "done"},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "stream.jsonl"
            path.write_text("".join(json.dumps(line) + "\n" for line in lines) + "not json\n")
            s = at.parse_stream(path)
        self.assertEqual(s["kc_calls"], ["mcp__knowcode__kc_refs", "Bash: knowcode"])
        self.assertEqual(s["plain_calls"], {"Bash": 1, "Read": 1})
        self.assertEqual(s["meta_calls"], {"Skill": ["knowcode:knowcode-explore"]})
        self.assertEqual((s["kc_call_count"], s["plain_call_count"]), (2, 2))
        self.assertAlmostEqual(s["cost_usd"], 0.0421)
        self.assertEqual(s["tools_offered"], ["Bash", "Read", "mcp__knowcode__kc_refs"])


class LedgerTest(unittest.TestCase):
    def test_spend_accumulates_and_numbers_runs(self):
        with tempfile.TemporaryDirectory() as tmp:
            work = Path(tmp)
            self.assertEqual(at.ledger_total(work), 0.0)
            at.record_spend(work, "before", "mcp", "q1", 0.25)
            at.record_spend(work, "before", "mcp", "q1", 0.5)
            self.assertAlmostEqual(at.record_spend(work, "before", "plugin", "q1", 0.125), 0.875)
            rows = [line.split("\t") for line in (work / "ledger.tsv").read_text().splitlines()]
            self.assertEqual(rows[0][:5], ["label", "config", "question", "run", "cost_usd"])
            self.assertEqual([r[3] for r in rows[1:]], ["1", "2", "1"])
            self.assertAlmostEqual(at.ledger_total(work), 0.875)


class SessionTest(unittest.TestCase):
    def test_argv_is_headless_isolated_and_names_no_tool_in_the_prompt(self):
        for config in at.CONFIGS:
            argv = at.session_argv("claude", "Who calls X?", config, model="m", effort="medium", max_usd=1,
                                   settings=Path("/s.json"), mcp=Path("/m.json"), knowcode=Path("/bin/knowcode"))
            self.assertEqual(argv[:4], ["claude", "--bare", "--print", "Who calls X?"])
            for flag in ("--strict-mcp-config", "--no-session-persistence", "--max-budget-usd"):
                self.assertIn(flag, argv)
            self.assertEqual(argv[argv.index("--setting-sources") + 1], "")
            self.assertIn("mcp__knowcode__kc_note_add", argv[argv.index("--disallowedTools"):])
            self.assertEqual("--plugin-dir" in argv, config == "plugin")
        full = at.session_argv("claude", "q", "plugin", model="m", effort="e", max_usd=1, settings=Path("/s"),
                               mcp=Path("/m"), knowcode=Path("/k"), bare=False)
        self.assertNotIn("--bare", full)
        self.assertEqual(full[full.index("--tools") + 1], "Bash,Read,Skill,ToolSearch")
        self.assertIn("ToolSearch", full[full.index("--allowedTools"):])
        with self.assertRaises(ValueError):
            at.session_argv("claude", "q", "nope", model="m", effort="e", max_usd=1, settings=Path("/s"),
                            mcp=Path("/m"), knowcode=Path("/k"))

    def test_env_is_minimal_and_carries_the_key_only_from_the_environment(self):
        with tempfile.TemporaryDirectory() as tmp, mock.patch.dict(os.environ, {"ANTHROPIC_API_KEY": "k", "OTHER": "x"}):
            env = at.session_env(Path(tmp) / "s", Path("/opt/kc/knowcode"), Path(tmp) / "cache", "/usr/local/bin/claude")
        self.assertEqual(env["ANTHROPIC_API_KEY"], "k")
        self.assertNotIn("OTHER", env)
        self.assertTrue(env["HOME"].startswith(tmp) and env["CLAUDE_CONFIG_DIR"].startswith(tmp))
        self.assertTrue(all(env[k].startswith(tmp) for k in ("KNOWCODE_CACHE_DIR", "KNOWCODE_CONFIG_DIR", "KNOWCODE_DATA_DIR")))
        self.assertEqual(env["PATH"].split(os.pathsep)[0], "/opt/kc")

    def test_run_refuses_without_a_key(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(at.main(["run", "--repos", "r", "--knowcode", "k", "--work", "w", "--label", "l",
                                      "--budget-usd", "1"]), 2)


class QuestionsTest(unittest.TestCase):
    def test_tuning_questions_are_not_the_study_questions(self):
        questions = at.load_questions(at.QUESTIONS)
        self.assertGreaterEqual(len(questions), 6)
        study = [json.loads(line) for line in STUDY.read_text().splitlines() if line.strip()]
        study_text = {q["question"] for q in study}
        study_repos = {r for q in study for r in q["repos"]}
        for q in questions:
            self.assertNotIn(q["question"], study_text)
            self.assertNotIn(q["repo"], study_repos)
            self.assertNotIn("knowcode", q["question"].lower())
            self.assertNotIn("kc_", q["question"])


if __name__ == "__main__":
    unittest.main()
