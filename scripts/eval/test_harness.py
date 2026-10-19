#!/usr/bin/env python3
"""Unit tests for the evaluation harness (synthetic data and a stand-in shell script for the binary; no corpus, no
knowcode build): python3 scripts/eval/test_harness.py"""

import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import snapshot  # noqa: E402
from common import bootstrap, is_test_path, json_path, normalize, wilson  # noqa: E402
from defects import auc, is_fix  # noqa: E402
from labels import score  # noqa: E402
from replay import clean_subject, expand_argv, ranked_files, score_task, tasks_for, terms  # noqa: E402
from snapshot import diff_values  # noqa: E402


class Normalize(unittest.TestCase):
    def test_drops_volatile_fields_and_row_ids(self):
        raw = {
            "knowcode": {"build": "abc"}, "freshness": {"check_ms": 1.2}, "stats": {"ms": 3},
            "items": [{"id": 7, "symbol_id": 9, "name": "Foo", "total_ms": 4.0, "kind_id": "keep"}],
        }
        self.assertEqual(normalize(raw), {"items": [{"name": "Foo", "kind_id": "keep"}]})

    def test_sorts_unordered_lists_and_keeps_ranked_ones(self):
        raw = {"items": [{"f": "b"}, {"f": "a"}], "other": [3, 1, 2]}
        self.assertEqual(normalize(raw)["items"], [{"f": "a"}, {"f": "b"}])
        self.assertEqual(normalize(raw, ordered={"/items"})["items"], [{"f": "b"}, {"f": "a"}])
        self.assertEqual(normalize(raw)["other"], [1, 2, 3])

    def test_rewrites_roots_and_rounds_floats(self):
        out = normalize({"repo": "/x/corpus/a", "r": 0.1153846153846154}, roots=[("<root>", "/x/corpus")])
        self.assertEqual(out, {"repo": "<root>/a", "r": 0.115385})
        self.assertEqual(normalize({"r": 0.11538461538461539}), {"r": 0.115385})


class JsonPath(unittest.TestCase):
    def test_walks_lists_and_skips_missing_keys(self):
        ans = {"items": [{"affected": [{"file": "a"}, {"file": "b"}]}, {"affected": [{"x": 1}]}, {"co_change": {"file": "c"}}]}
        self.assertEqual(json_path(ans, "items[].affected[].file"), ["a", "b"])
        self.assertEqual(json_path(ans, "items[].co_change.file"), ["c"])
        self.assertEqual(json_path(ans, "nothing[].file"), [])


class TestPaths(unittest.TestCase):
    def test_mirrors_the_store_rule(self):
        for p in ["tests/Api.Tests/Hooks/Setup.cs", "src/Api.UnitTests/Setup.cs", "src/Api/PlacedConsumerTests.cs",
                  "a/foo.spec.ts", "test_api.py", "pkg/foo_test.go", "conftest.py", "src/__tests__/x.ts"]:
            self.assertTrue(is_test_path(p), p)
        for p in ["src/Api/Startup.cs", "src/Api/Consumers/MonitorTestFailedConsumer.cs", "Latest.cs",
                  "src/Testimonial.ts", "src/DeleteState.cs"]:
            self.assertFalse(is_test_path(p), p)


class Stats(unittest.TestCase):
    def test_wilson(self):
        lo, hi = wilson(50, 100)
        self.assertAlmostEqual(lo, 0.4038, places=3)
        self.assertAlmostEqual(hi, 0.5962, places=3)
        self.assertEqual(wilson(0, 0), (0.0, 0.0))

    def test_bootstrap_brackets_the_mean(self):
        vals = [0, 1] * 50
        lo, hi = bootstrap(vals, groups=[i % 10 for i in range(100)])
        self.assertTrue(lo <= 0.5 <= hi)


def commit(files):
    return {"repo": "r", "sha": "s", "subject": "x", "files": [{"path": p, "status": st} for p, st in files]}


class Replay(unittest.TestCase):
    def test_tasks_leave_out_files_added_by_the_commit(self):
        c = commit([("src/A.cs", "M"), ("src/B.cs", "M"), ("src/New.cs", "A"), ("tests/ATests.cs", "M")])
        tasks = tasks_for({"task": "per_file", "input": "source", "truth": "others"}, c)
        self.assertEqual(tasks, [(["src/A.cs"], {"src/B.cs", "tests/ATests.cs"}), (["src/B.cs"], {"src/A.cs", "tests/ATests.cs"})])
        self.assertEqual(tasks_for({"task": "per_commit", "input": "source", "truth": "tests"}, c),
                         [(["src/A.cs", "src/B.cs"], {"tests/ATests.cs"})])
        self.assertEqual(tasks_for({"task": "per_commit", "input": "none", "truth": "others"}, c),
                         [([], {"src/A.cs", "src/B.cs", "tests/ATests.cs"})])

    def test_renamed_files_count_at_their_old_path(self):
        c = {"files": [{"path": "src/New.cs", "old": "src/Old.cs", "status": "R"}, {"path": "src/B.cs", "status": "M"}]}
        self.assertEqual(tasks_for({"input": "source"}, c)[0], (["src/Old.cs"], {"src/B.cs"}))

    def test_score_task(self):
        row = score_task(["a", "x", "b", "y"], {"a", "b", "c"}, [1, 2, 5])
        self.assertEqual(row["hits@1"], 1)
        self.assertAlmostEqual(row["recall@2"], 1 / 3)
        self.assertEqual(row["precision@2"], 0.5)
        self.assertAlmostEqual(row["recall@5"], 2 / 3)
        self.assertEqual(row["precision@5"], 0.5)  # 2 hits in an answer of 4
        self.assertEqual(score_task([], {"a"}, [5])["precision@5"], None)

    def test_ranked_files_dedupes_and_filters(self):
        ans = {"items": [{"file": "a.cs"}, {"file": "tests/BTests.cs"}, {"file": "a.cs"}]}
        self.assertEqual(ranked_files(ans, "items[].file", "all"), ["a.cs", "tests/BTests.cs"])
        self.assertEqual(ranked_files(ans, "items[].file", "tests"), ["tests/BTests.cs"])
        two = {"items": [{"affected": [{"file": "a.cs"}], "coupled": [{"file": "b.cs"}, {"file": "a.cs"}]}]}
        self.assertEqual(ranked_files(two, ["items[].affected[].file", "items[].coupled[].file"], "all"), ["a.cs", "b.cs"])

    def test_files_placeholder_expands_to_one_argument_each(self):
        self.assertEqual(expand_argv(["tests", "{files}", "-n", "{file}"], {"file": "x"}, ["a.cs", "b.cs"]), ["tests", "a.cs", "b.cs", "-n", "x"])

    def test_subject_cleaning(self):
        self.assertEqual(clean_subject("feat(api): [PAY-123] Add refund endpoint (#45)"), "Add refund endpoint")
        self.assertEqual(clean_subject("sc-4521 fix the cart total"), "fix the cart total")
        self.assertEqual(terms("Add refund endpoint for the cart"), ["refund", "endpoint", "cart"])


class Labels(unittest.TestCase):
    def test_rules(self):
        tp, fp, fn = score(["a.cs:10", "a.cs:30", "b.cs:5"], ["a.cs:11", "c.cs:1"], "line_within:2")
        self.assertEqual((tp, fp, fn), (["a.cs:10"], ["a.cs:30", "b.cs:5"], ["c.cs:1"]))
        self.assertEqual(score(["Ns.Foo.Bar"], ["Foo.Bar"], "suffix")[0], ["Ns.Foo.Bar"])
        self.assertEqual(score(["NsFoo.Bar"], ["Foo.Bar"], "suffix")[0], [])
        self.assertEqual(score(["a.cs:3:kind"], ["a.cs:3"], "file_line")[0], ["a.cs:3:kind"])
        self.assertEqual(score(["a.cs:3"], ["a.cs:9"], "file")[0], ["a.cs:3"])

    def test_each_expected_item_matches_once(self):
        tp, fp, fn = score(["a.cs:1", "a.cs:2"], ["a.cs:1"], "file")
        self.assertEqual((len(tp), len(fp), len(fn)), (1, 1, 0))


class Diff(unittest.TestCase):
    def lines(self, a, b):
        out = []
        diff_values(a, b, "", out)
        return out

    def test_changed_row_shows_the_field(self):
        a = {"items": [{"file": "a.cs", "line": 1, "confidence": "exact"}, {"file": "b.cs", "line": 2}]}
        b = {"items": [{"file": "a.cs", "line": 1, "confidence": "ambiguous"}, {"file": "c.cs", "line": 3}]}
        out = self.lines(a, b)
        self.assertIn("  ~ /items[file=a.cs line=1]/confidence: \"exact\" -> \"ambiguous\"", out)
        self.assertIn("  - /items[]: file=b.cs line=2", out)
        self.assertIn("  + /items[]: file=c.cs line=3", out)

    def test_reorder_and_identity(self):
        self.assertEqual(self.lines({"x": [1, 2]}, {"x": [1, 2]}), [])
        self.assertEqual(self.lines({"x": [1, 2]}, {"x": [2, 1]}), ["  ~ /x: same 2 rows, reordered"])



def write_snapshot(d, answers, **meta):
    """A finished snapshot in `d`: one file per answer and a `_meta.json` (fields overridable)."""
    d.mkdir(parents=True, exist_ok=True)
    for qid, ans in answers.items():
        (d / f"{qid}.json").write_text(json.dumps(ans))
    m = {"count": len(answers), "errors": 0, "failed": [], "valid": True, "format": snapshot.FORMAT}
    m.update(meta)
    (d / "_meta.json").write_text(json.dumps(m))


def run_diff(a, b):
    """`diff a b`: (exit code, output)."""
    ns = type("A", (), {"a": str(a), "b": str(b), "max": 5})
    with redirect_stdout(io.StringIO()) as buf, unittest.TestCase().assertRaises(SystemExit) as e:
        snapshot.diff(ns)
    return e.exception.code, buf.getvalue()


# A stand-in for the binary: answers each command with its argv, its working directory and its config directory as
# JSON; `boom` fails, `refuse` answers and exits 2 (as `rename` does when it refuses), `slow` prints no JSON.
FAKE_BIN = """#!/bin/sh
case "$1" in
  boom) echo "knowcode: no such symbol" >&2; exit 3 ;;
  refuse) echo '{"items": [{"refused": true}]}'; exit 2 ;;
  slow) echo 'not json'; exit 0 ;;
  --version) echo 'knowcode test'; exit 0 ;;
esac
printf '{"argv": "%s", "cwd": "%s", "config": "%s", "items": [{"f": "b"}, {"f": "a"}]}\\n' "$*" "$PWD" "$KNOWCODE_CONFIG_DIR"
"""


class Snapshot(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.t = Path(self.tmp.name).resolve()
        self.env = dict(os.environ)  # `snapshot` sets KNOWCODE_NOW and KNOWCODE_CONFIG_DIR for its run

    def tearDown(self):
        os.environ.clear()
        os.environ.update(self.env)
        self.tmp.cleanup()

    def test_ranked_commands_keep_their_order_by_default(self):
        for cmd in ["locate", "tests", "coupled", "hotspots", "reviewers", "find", "search", "ownership", "impact",
                    "path", "flow", "at", "overview", "grep-ast", "note list"]:
            self.assertEqual(snapshot.order_rules({"cmd": cmd}), (True, set(), set()), cmd)
        self.assertEqual(snapshot.order_rules({"cmd": "refs"}), (False, set(), set()))
        self.assertEqual(snapshot.order_rules({"cmd": "trace", "ordered": ["/items"]}), (False, {"/items"}, set()))
        self.assertEqual(snapshot.order_rules({"cmd": "find", "unordered": ["/x"]}), (True, set(), {"/x"}))
        self.assertEqual(snapshot.order_rules({"cmd": "find", "unordered": True}), (False, set(), set()))

    def test_normalize_keeps_order_except_where_told(self):
        raw = {"items": [{"f": "b", "tags": [2, 1]}, {"f": "a", "tags": [4, 3]}]}
        kept = normalize(raw, keep_order=True, unordered={"/items[]/tags"})
        self.assertEqual(kept, {"items": [{"f": "b", "tags": [1, 2]}, {"f": "a", "tags": [3, 4]}]})
        self.assertEqual(normalize(raw, keep_order=True)["items"][0]["tags"], [2, 1])

    def test_a_reordered_ranking_is_a_difference(self):
        write_snapshot(self.t / "a", {"q": {"items": ["x", "y"]}})
        write_snapshot(self.t / "b", {"q": {"items": ["y", "x"]}})
        code, out = run_diff(self.t / "a", self.t / "b")
        self.assertEqual(code, 1)
        self.assertIn("same 2 rows, reordered", out)

    def test_diff_exit_codes(self):
        write_snapshot(self.t / "a", {"q1": {"total": 1}})
        write_snapshot(self.t / "b", {"q1": {"total": 2}})
        write_snapshot(self.t / "c", {"q1": {"total": 1}})
        code, out = run_diff(self.t / "a", self.t / "b")
        self.assertEqual(code, 1)
        self.assertIn("q1: 1 difference", out)
        self.assertEqual(run_diff(self.t / "a", self.t / "c")[0], 0)

    def test_diff_refuses_a_snapshot_holding_errors(self):
        err = {"error": "knowcode: boom", "exit": 3}
        write_snapshot(self.t / "ok", {"q1": {"total": 1}, "q2": err})
        # The same error on both sides is still no clean diff.
        write_snapshot(self.t / "same", {"q1": {"total": 1}, "q2": err})
        code, out = run_diff(self.t / "ok", self.t / "same")
        self.assertEqual(code, 2)
        self.assertIn("queries failed: q2", out)
        # Errors recorded in the meta file alone count too.
        write_snapshot(self.t / "m1", {"q1": {"total": 1}}, errors=1, failed=["q9"], valid=False)
        write_snapshot(self.t / "m2", {"q1": {"total": 1}})
        self.assertEqual(run_diff(self.t / "m2", self.t / "m1")[0], 2)

    def test_diff_refuses_unfinished_or_incompatible_snapshots(self):
        write_snapshot(self.t / "good", {"q1": {"total": 1}})
        bare = self.t / "bare"
        bare.mkdir()
        (bare / "q1.json").write_text(json.dumps({"total": 1}))
        code, out = run_diff(self.t / "good", bare)
        self.assertEqual(code, 2)
        self.assertIn("no _meta.json", out)
        write_snapshot(self.t / "old", {"q1": {"total": 1}}, format=1)
        code, out = run_diff(self.t / "old", self.t / "good")
        self.assertEqual(code, 2)
        self.assertIn("normalisation format 1", out)
        write_snapshot(self.t / "short", {"q1": {"total": 1}}, count=2)
        code, out = run_diff(self.t / "good", self.t / "short")
        self.assertEqual(code, 2)
        self.assertIn("1 answers, but _meta.json says 2", out)

    def fake(self):
        bin_ = self.t / "fake-knowcode"
        bin_.write_text(FAKE_BIN)
        bin_.chmod(0o755)
        return bin_

    def take(self, queries, out="snap", config=None):
        (self.t / "root" / "r1").mkdir(parents=True, exist_ok=True)
        qs = self.t / "queries.toml"
        qs.write_text(f'root = "{self.t / "root"}"\n' + queries)
        ns = type("A", (), {"queries": str(qs), "bin": self.fake(), "cache": str(self.t / "cache"),
                            "out": str(self.t / out), "index": False, "only": None, "jobs": 2, "threads": 1,
                            "config": config})
        code = 0
        with redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()) as err:
            try:
                snapshot.snapshot(ns)
            except SystemExit as e:
                code = e.code
        return code, err.getvalue(), self.t / out

    def test_a_failed_query_makes_the_snapshot_invalid(self):
        code, err, out = self.take('[[query]]\nid = "fine"\nrepo = "r1"\ncmd = "find"\nargs = ["X"]\n'
                                   '[[query]]\nid = "broken"\nrepo = "r1"\ncmd = "boom"\n'
                                   '[[query]]\nid = "garbled"\nrepo = "r1"\ncmd = "slow"\n')
        self.assertEqual(code, 1)
        self.assertIn("broken: error (exit 3): knowcode: no such symbol", err)
        meta = json.loads((out / "_meta.json").read_text())
        self.assertEqual((meta["valid"], meta["errors"], sorted(meta["failed"])), (False, 2, ["broken", "garbled"]))
        self.assertEqual(run_diff(out, out)[0], 2)

    def test_a_query_runs_as_its_fields_say(self):
        code, _, out = self.take('[[query]]\nid = "ranked"\nrepo = "r1"\ncmd = "find"\nargs = ["X"]\n'
                                 '[[query]]\nid = "set"\nrepo = "r1"\ncmd = "refs"\nargs = ["X"]\n'
                                 '[[query]]\nid = "refused"\nrepo = "r1"\ncmd = "refuse"\nexit = 2\n'
                                 '[[query]]\nid = "everywhere"\nrepos = ["r1"]\nscope = "workspace"\ncmd = "note list"\n')
        self.assertEqual(code, 0)
        ans = {p.stem: json.loads(p.read_text()) for p in out.glob("*.json")}
        self.assertEqual(ans["ranked"]["items"], [{"f": "b"}, {"f": "a"}])
        self.assertEqual(ans["set"]["items"], [{"f": "a"}, {"f": "b"}])
        self.assertEqual(ans["refused"], {"items": [{"refused": True}]})
        self.assertEqual(ans["ranked"]["argv"], "find X --json --no-sync --repo <root>/r1")
        # A workspace question names no repo and runs from the root.
        self.assertEqual((ans["everywhere"]["argv"], ans["everywhere"]["cwd"]), ("note list --json --no-sync", "<root>"))
        # The run's config directory is its own, never the user's.
        self.assertEqual(ans["ranked"]["config"], "<cache>/_config")
        self.assertTrue(json.loads((out / "_meta.json").read_text())["valid"])
        # Taking it again over the same directory replaces every answer; an exit the query doesn't name is an error.
        code, _, out = self.take('[[query]]\nid = "md"\nrepo = "r1"\ncmd = "docs"\ntext = true\n'
                                 '[[query]]\nid = "sized"\nrepo = "r1"\ncmd = "find"\ndrop = ["/items[]/f", "/cwd"]\n')
        self.assertEqual(code, 0)
        md = json.loads((out / "md.json").read_text())["text"]
        self.assertTrue(md.startswith('{"argv": "docs --no-sync --repo <root>/r1"'), md)
        sized = json.loads((out / "sized.json").read_text())
        self.assertEqual((sized["items"], "cwd" in sized), ([{}, {}], False))
        code, _, out = self.take('[[query]]\nid = "refused"\nrepo = "r1"\ncmd = "refuse"\n')
        self.assertEqual(code, 1)
        self.assertEqual(sorted(p.name for p in out.glob("*.json")), ["_meta.json", "refused.json"])

    def test_a_config_dir_given_overrides_the_query_sets(self):
        given = self.t / "team-config"
        given.mkdir()
        code, _, out = self.take(f'config = "{self.t / "set-config"}"\n[[query]]\nid = "q"\nrepo = "r1"\ncmd = "find"\n',
                                 config=str(given))
        self.assertEqual(code, 0)
        self.assertEqual(json.loads((out / "q.json").read_text())["config"], str(given))
        self.assertEqual(json.loads((out / "_meta.json").read_text())["config"], str(given))
        code, _, out = self.take(f'config = "{self.t / "set-config"}"\n[[query]]\nid = "q"\nrepo = "r1"\ncmd = "find"\n', out="s2")
        self.assertEqual(json.loads((out / "q.json").read_text())["config"], str(self.t / "set-config"))



class Defects(unittest.TestCase):
    def test_fix_rule_mirrors_the_binary(self):
        # The cases of vitals::tests::fix_words and fix_conventions.
        for subject in ["Fix a bug in the parser", 'Revert "Add x"', "hotfix: crash", "fix: invalidate the cache after checkout",
                        "fix(grid)!: render rows after the upgrade", "sc-4521: fix batch import crash on duplicate rows",
                        "[BUG] totals off by one", "Fixed form field issues", "Fix broken links", "Correct inflated revenue for renewals"]:
            self.assertTrue(is_fix(subject), subject)
        for subject in ["Fix GPS fix handling", "Debug overlay", "feat: show a fixed footer", "test: add regression test for filters",
                        "chore(deps): bump to fix advisory", "fix tests", "lint fixes", "fix merge conflicts", "disable broken tests",
                        "Fix typo in README", "Resolve the decrypter immediately", "Save the update script correctly", "Add a prefix tree"]:
            self.assertFalse(is_fix(subject), subject)

    def test_auc_counts_ties_half(self):
        self.assertEqual(auc([3, 2, 1], [True, False, False]), 1.0)
        self.assertEqual(auc([1, 2, 3], [True, False, False]), 0.0)
        self.assertEqual(auc([1, 1], [True, False]), 0.5)
        self.assertIsNone(auc([1, 2], [False, False]))


if __name__ == "__main__":
    unittest.main()
