"""Tests for clean_machine.py: the transcript parser, the per-entry settings comparison and the registration check.

python3 -m unittest discover -s scripts/acceptance -p 'test_*.py'
"""
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import clean_machine as cm  # noqa: E402

QUESTION = "Where is mitt defined, and how does it initialize its event-handler map?"


def line(kind, content, ts, cwd, **extra):
    role = "user" if kind == "user" else "assistant"
    return {"type": kind, "sessionId": extra.pop("session", "s1"), "cwd": cwd, "timestamp": ts,
            "message": {"role": role, "content": content}, **extra}


def tool(name, **inp):
    return {"type": "tool_use", "id": name, "name": name, "input": inp}


class TranscriptTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.case = self.root / "repos" / "mitt"
        self.case.mkdir(parents=True)
        self.projects = self.root / "projects"

    def tearDown(self):
        self.tmp.cleanup()

    def write(self, name, entries, project="-repos-mitt"):
        path = self.projects / project / f"{name}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("".join(json.dumps(e) + "\n" for e in entries) + "not json\n")
        return path

    def session(self, prompt, tools, session="s1"):
        cwd = str(self.case)
        entries = [
            {"type": "file-history-snapshot", "snapshot": {}},
            line("user", "<command-name>/clear</command-name>", "2026-10-20T10:00:00.000Z", cwd, session=session),
            line("user", prompt, "2026-10-20T10:00:01.000Z", cwd, session=session),
            line("assistant", [{"type": "thinking", "thinking": "..."}, *tools], "2026-10-20T10:00:05.000Z", cwd,
                 session=session),
            line("user", [{"type": "tool_result", "tool_use_id": "x", "content": "ok"}], "2026-10-20T10:00:06.000Z",
                 cwd, session=session),
            line("assistant", [tool("Read", file_path="x")], "2026-10-20T10:00:07.000Z", cwd, session=session,
                 isSidechain=True),
            line("assistant", [{"type": "text", "text": "Interim."}], "2026-10-20T10:00:08.000Z", cwd, session=session),
            line("assistant", [{"type": "text", "text": "mitt is defined in src/index.ts."}],
                 "2026-10-20T10:00:31.500Z", cwd, session=session),
        ]
        return entries

    def test_parse_counts_knowcode_and_other_calls(self):
        path = self.write("a", self.session(QUESTION, [
            tool("mcp__plugin_knowcode_knowcode__kc_context", symbol="mitt"),
            tool("Bash", command="knowcode find mitt"),
            tool("Bash", command="grep -rn mitt src"),
            tool("Grep", pattern="mitt"),
        ]))
        s = cm.parse_transcript(path)
        self.assertEqual(s["prompt"], QUESTION)
        self.assertTrue(s["used_knowcode"])
        self.assertEqual(s["kc_call_count"], 2)
        self.assertEqual(s["other_tool_calls"], {"Bash": 1, "Grep": 1, "Read": 1})
        self.assertEqual(s["subagent_tool_calls"], 1)
        self.assertEqual(s["final_answer"], "mitt is defined in src/index.ts.")
        self.assertEqual(s["seconds_to_final_answer"], 30.5)

    def test_parse_without_knowcode(self):
        s = cm.parse_transcript(self.write("b", self.session(QUESTION, [tool("Grep", pattern="mitt")])))
        self.assertFalse(s["used_knowcode"])
        self.assertEqual(s["kc_call_count"], 0)
        # A path that merely contains "knowcode" is not a knowcode call.
        self.assertIsNone(cm.is_knowcode_tool("Bash", {"command": "ls ~/knowcode-acceptance"}))
        self.assertEqual(cm.is_knowcode_tool("Bash", {"command": "cd x && knowcode refs mitt"}), "cli")

    def test_prompt_given_as_text_blocks(self):
        entries = self.session("ignored", [])
        entries[2]["message"]["content"] = [{"type": "text", "text": "  " + QUESTION + "\n"}]
        self.assertEqual(cm.parse_transcript(self.write("c", entries))["prompt"], QUESTION)

    def test_script_asks_the_named_prompt_the_parser_looks_for(self):
        script = (Path(__file__).resolve().parent / "clean-machine.sh").read_text()
        self.assertIn('prompt="' + cm.NAMED_PROMPT.format(question="$question") + '"', script)
        self.assertNotIn("kc_", cm.NAMED_PROMPT)

    def test_find_sessions_matches_prompt_dir_and_time(self):
        named = cm.NAMED_PROMPT.format(question=QUESTION)
        self.write("unprompted", self.session(QUESTION, [tool("Grep", pattern="mitt")], "u"))
        self.write("named", self.session(named, [tool("mcp__plugin_knowcode_knowcode__kc_context")], "n"))
        other = self.root / "repos" / "other"
        other.mkdir()
        elsewhere = self.session(QUESTION, [tool("mcp__plugin_knowcode_knowcode__kc_find")], "o")
        for e in elsewhere:
            if "cwd" in e:
                e["cwd"] = str(other)
        self.write("elsewhere", elsewhere, project="-repos-other")
        found = cm.find_sessions(self.projects, self.case, {"unprompted": QUESTION, "named": named})
        self.assertEqual(found["unprompted"]["session_id"], "u")
        self.assertFalse(found["unprompted"]["used_knowcode"])
        self.assertEqual(found["named"]["session_id"], "n")
        self.assertTrue(found["named"]["used_knowcode"])
        later = cm.find_sessions(self.projects, self.case, {"unprompted": QUESTION}, since=time.time() + 60)
        self.assertIsNone(later["unprompted"])

    def test_subagent_transcripts_are_counted(self):
        self.write("s1", self.session(QUESTION, []))
        sub = self.projects / "-repos-mitt" / "s1" / "subagents" / "agent-1.jsonl"
        sub.parent.mkdir(parents=True)
        sub.write_text(json.dumps(line("assistant", [tool("mcp__plugin_knowcode_knowcode__kc_refs")],
                                       "2026-10-20T10:00:09Z", str(self.case), isSidechain=True)) + "\n")
        s = cm.parse_transcript(self.projects / "-repos-mitt" / "s1.jsonl")
        self.assertTrue(s["used_knowcode"])
        self.assertEqual(s["kc_calls"], ["mcp__plugin_knowcode_knowcode__kc_refs"])

    def test_sessions_command_reports_missing(self):
        out = self.root / "out.json"
        import contextlib
        import io
        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            cm.main(["sessions", "--projects", str(self.projects), "--case-dir", str(self.case), "--question", QUESTION])
        report = json.loads(buffer.getvalue())
        self.assertEqual(report["unprompted"], {"prompt": QUESTION, "found": False})
        out.write_text(buffer.getvalue())
        with contextlib.redirect_stdout(io.StringIO()) as line_out:
            cm.main(["session-line", str(out)])
        self.assertIn("unprompted: found=False", line_out.getvalue())

    def test_use_record_counts_knowcode_calls_per_case_and_session(self):
        report = self.root / "report"
        report.mkdir()
        (report / "sessions-mitt.json").write_text(json.dumps({
            "unprompted": {"found": True, "prompt": QUESTION, "used_knowcode": True, "kc_call_count": 3,
                           "kc_calls": ["mcp__plugin_knowcode_knowcode__kc_context",
                                        "mcp__plugin_knowcode_knowcode__kc_context", "Bash: knowcode refs mitt"],
                           "other_tool_call_count": 2},
            "named": {"found": False, "prompt": cm.NAMED_PROMPT.format(question=QUESTION)}}))
        use = cm.knowcode_use(report)
        self.assertEqual(use["format"], 1)
        self.assertEqual(use["sessions"][0], {"case": "mitt", "session": "named", "question": use["sessions"][0]["question"],
                                              "found": False})
        self.assertEqual(use["sessions"][1]["kc_calls_by_tool"], {"kc_context": 2, "cli": 1})
        self.assertEqual(use["sessions"][1]["other_tool_calls"], 2)
        self.assertEqual(use["unprompted"], {"sessions": 1, "using_knowcode": 1, "kc_calls": 3})


BEFORE = {
    "model": "opus",
    "permissions": {"allow": ["Bash(ls:*)"], "deny": []},
    "enabledPlugins": {"knowcode@knowcode-local": True, "other@market": True},
    "extraKnownMarketplaces": {"knowcode-local": {"source": {"source": "directory", "path": "/x"}},
                               "market": {"source": {"source": "github", "repo": "o/r"}}},
}


class SettingsTest(unittest.TestCase):
    def after(self, **changes):
        after = json.loads(json.dumps(BEFORE))
        del after["enabledPlugins"]["knowcode@knowcode-local"]
        del after["extraKnownMarketplaces"]["knowcode-local"]
        after.update(changes)
        return after

    def test_only_knowcode_entries_removed_is_preserved(self):
        result = cm.compare_settings(BEFORE, self.after())
        self.assertTrue(result["preserved"], result)
        self.assertEqual(result["knowcode_entries_removed"],
                         ["enabledPlugins['knowcode@knowcode-local']", "extraKnownMarketplaces['knowcode-local']"])

    def test_keys_holding_other_entries_are_not_reported_whole(self):
        after = self.after()
        after["enabledPlugins"]["other@market"] = False
        result = cm.compare_settings(BEFORE, after)
        self.assertFalse(result["preserved"])
        self.assertEqual([d["entry"] for d in result["unrelated_differences"]], ["enabledPlugins['other@market']"])
        self.assertEqual(result["unrelated_differences"][0]["change"], "changed")

    def test_dropping_a_whole_key_fails(self):
        after = self.after()
        del after["extraKnownMarketplaces"]
        result = cm.compare_settings(BEFORE, after)
        self.assertEqual([(d["entry"], d["change"]) for d in result["unrelated_differences"]],
                         [("extraKnownMarketplaces['market']", "removed")])

    def test_emptied_key_may_be_dropped(self):
        before = {"enabledPlugins": {"knowcode@knowcode-local": True}, "theme": "dark"}
        for after in ({"theme": "dark"}, {"enabledPlugins": {}, "theme": "dark"}):
            self.assertTrue(cm.compare_settings(before, after)["preserved"], after)

    def test_knowcode_entry_left_behind_fails(self):
        result = cm.compare_settings(BEFORE, BEFORE)
        self.assertFalse(result["preserved"])
        self.assertEqual(result["unrelated_differences"], [])
        self.assertEqual(len(result["knowcode_entries_remaining"]), 2)

    def test_scalar_and_added_keys(self):
        result = cm.compare_settings(BEFORE, self.after(model="sonnet", newKey=1))
        self.assertEqual(sorted((d["entry"], d["change"]) for d in result["unrelated_differences"]),
                         [("model", "changed"), ("newKey", "added")])

    def test_missing_files(self):
        self.assertTrue(cm.compare_settings(None, None)["preserved"])


class RegistrationTest(unittest.TestCase):
    def test_registrations(self):
        plugins = [{"id": "knowcode@knowcode-local", "enabled": True}, {"id": "other@market"}]
        markets = [{"name": "knowcode-local"}, {"name": "market"}]
        self.assertEqual(cm.registrations(plugins, markets), ["plugin knowcode@knowcode-local",
                                                              "marketplace knowcode-local"])
        self.assertEqual(cm.registrations(None, None), [])
        self.assertEqual(cm.settings_entries(BEFORE), ["enabledPlugins['knowcode@knowcode-local']",
                                                       "extraKnownMarketplaces['knowcode-local']"])

    def test_environment_labels(self):
        self.assertEqual(cm.environment_label("Darwin", "arm64"), "fresh-macos-account")
        self.assertEqual(cm.environment_label("Linux", "x86_64"), "clean-linux-x86_64-container")
        self.assertEqual(cm.environment_label("Linux", "aarch64"), "clean-linux-arm64")
        self.assertEqual(cm.environment_label("Darwin", "x86_64"), "isolated-profile")

    def test_machine_record(self):
        record = cm.machine_record("yes", "note", "2.0.0 (Claude Code)")
        self.assertTrue(record["quiet_confirmed_by_operator"])
        self.assertEqual(record["client_version"], "2.0.0 (Claude Code)")
        self.assertEqual(len(record["load_average"]), 3)


if __name__ == "__main__":
    os.environ.setdefault("PYTHONDONTWRITEBYTECODE", "1")
    unittest.main()
