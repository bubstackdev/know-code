#!/usr/bin/env python3
"""Unit tests for the neutral benchmark's sampler, schema and scorer (synthetic fixtures; no corpus, no tool):
python3 scripts/eval/neutral/test_neutral.py"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import sample  # noqa: E402
from schema import validate_bank, validate_label, validate_question  # noqa: E402
from score import score, score_one  # noqa: E402


def question(qid="Q1", **kw):
    q = {"id": qid, "stratum": "cs-callers", "source": "structural", "type": "callers", "question": "what calls X?",
         "repos": ["app"], "commit": {"app": "abc"}, "index_state": {"app": "abc"}, "answer_kind": "file_lines"}
    q.update(kw)
    return q


def label(qid="Q1", items=(), completeness="exhaustive", match="line_within:2", tests=()):
    return {"id": qid, "items": [{"item": i, "evidence": "e"} for i in items],
            "tests": [{"item": i, "evidence": "e"} for i in tests], "completeness": completeness, "match": match}


class Schema(unittest.TestCase):
    def test_accepts_a_well_formed_bank(self):
        self.assertEqual(validate_bank([question()], [label(items=["app/a.cs:3"])]), [])

    def test_flags_missing_fields_and_unknown_values(self):
        self.assertTrue(any("missing index_state" in e for e in validate_question(
            {k: v for k, v in question().items() if k != "index_state"})))
        self.assertTrue(validate_question(question(source="vibes")))
        self.assertTrue(validate_question(question(commit={})))
        self.assertTrue(validate_label(label(items=["x"], match="fuzzy")))
        self.assertTrue(validate_label(label(items=["x"], completeness="maybe")))

    def test_items_need_evidence_and_empty_answers_must_be_exhaustive(self):
        bad = label(items=["x"])
        bad["items"][0]["evidence"] = ""
        self.assertTrue(validate_label(bad))
        unsure = label(items=["x"])
        unsure["undecided"] = [{"item": "y"}]
        self.assertTrue(validate_label(unsure))
        self.assertEqual(validate_label(label(items=[], completeness="exhaustive")), [])
        self.assertTrue(validate_label(label(items=[], completeness="lower_bound")))

    def test_bank_pairs_questions_and_labels(self):
        errs = validate_bank([question("Q1"), question("Q2")], [label("Q1", ["a:1"]), label("Q3", ["a:1"])])
        self.assertIn("question Q2: no label", errs)
        self.assertIn("label Q3: no question", errs)


class Weights(unittest.TestCase):
    def test_allocation_sums_and_gives_every_stratum_its_minimum(self):
        alloc = sample.allocate({"a": 0.7, "b": 0.2, "c": 0.1, "d": 0.0}, 10)
        self.assertEqual(sum(alloc.values()), 10)
        self.assertTrue(all(v >= 1 for v in alloc.values()))
        self.assertGreater(alloc["a"], alloc["b"])
        self.assertEqual(sample.allocate({"a": 3, "b": 1}, 4, minimum=0), {"a": 3, "b": 1})
        with self.assertRaises(ValueError):
            sample.allocate({"a": 1, "b": 1, "c": 1}, 2)

    def test_capped_sample_keeps_one_per_cluster_and_uncapped_draws_unchanged(self):
        import random
        keys = [f"k{i}" for i in range(200)]
        plain = random.Random("s").sample(keys, 5)
        self.assertEqual(sample.capped_sample("s", keys, 5, lambda k: k), plain)
        got = sample.capped_sample("s", keys, 5, lambda k: int(k[1:]) % 3)
        self.assertEqual(len(got), 3)  # only three clusters exist
        self.assertEqual(len({int(k[1:]) % 3 for k in got}), 3)

    def test_weights_mix_file_and_line_shares(self):
        w = sample.stratum_weights({"x": [10, 1000], "y": [10, 0]})
        self.assertAlmostEqual(sum(w.values()), 1.0, places=3)
        self.assertAlmostEqual(w["x"], 0.75, places=3)


class Titles(unittest.TestCase):
    def test_cleans_branch_names_ticket_ids_and_prefixes(self):
        self.assertEqual(sample.clean_title("Merge branch 'feature/sc-123/fix-the-cart-total' into 'main'"),
                         "fix the cart total")
        self.assertEqual(sample.clean_title("Feature/sc 4567/add host to balancer pool"), "add host to balancer pool")
        self.assertEqual(sample.clean_title("feat(cart): [sc-9] round totals per line"), "round totals per line")
        self.assertEqual(sample.clean_title(":red_circle: Error queue growth"), "Error queue growth")

    def test_thin_titles_are_unusable(self):
        self.assertIsNone(sample.usable_title("WIP"))
        self.assertIsNone(sample.usable_title("Fix Concurrency Issues"))
        self.assertIsNone(sample.usable_title("Update file Program.cs and more stuff"))
        t = "Retry fetch on client timeout errors"
        self.assertEqual(sample.usable_title(t), t)

    def test_table_families_collapse_shards(self):
        self.assertEqual(sample.table_family("log_data_20100514_7163"), "log_data")
        self.assertEqual(sample.table_family("requests_2017_10"), "requests")
        self.assertEqual(sample.table_family("Orders"), "orders")


class Impact(unittest.TestCase):
    def test_declarations(self):
        self.assertEqual(sample.decl_name("    public async Task<int> Save(int id)"), ("Save", "int id"))
        self.assertIsNone(sample.decl_name("    public record Query(string a)"))
        self.assertIsNone(sample.decl_name("    var x = new Foo(1);"))
        self.assertEqual(sample.decl_name('<cffunction name="getUser" access="public">'), ("getUser", ""))

    def test_signature_change_with_updated_call_sites(self):
        diffs = {
            "Svc.cs": [("-", 10, "    public void Save(int id)"),
                       ("+", 10, "    public void Save(int id, bool force)")],
            "Caller.cs": [("-", 5, "        svc.Save(1);"), ("+", 5, "        svc.Save(1, true);"),
                          ("-", 9, "        // svc.Save(2);")],
        }
        c = sample.impact_candidates(diffs)
        self.assertEqual(len(c), 1)
        self.assertEqual((c[0]["name"], c[0]["new_name"], c[0]["file"], c[0]["line"]), ("Save", "Save", "Svc.cs", 10))
        self.assertEqual(c[0]["sites"], [("Caller.cs", 5)])

    def test_rename_and_unchanged_signature(self):
        renamed = {"A.ts": [("-", 3, "  private loadAll(x: number) {"), ("+", 3, "  private fetchAll(x: number) {"),
                            ("-", 20, "    this.loadAll(1);"), ("+", 20, "    this.fetchAll(1);")]}
        c = sample.impact_candidates(renamed)
        self.assertEqual((c[0]["name"], c[0]["new_name"], c[0]["sites"]), ("loadAll", "fetchAll", [("A.ts", 20)]))
        same = {"A.cs": [("-", 3, "  public void Go(int a)"), ("+", 3, "  public void Go(int a)"),
                         ("-", 9, "  Go(1);")]}
        self.assertEqual(sample.impact_candidates(same), [])


def git(d, *args, env=None):
    subprocess.run(["git", "-C", str(d), *args], check=True, capture_output=True,
                   env={**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", **(env or {})})


class History(unittest.TestCase):
    def commit(self, d, files, msg, author="Dev Person <dev@example.com>"):
        for name, text in files.items():
            p = d / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(text)
        git(d, "add", "-A")
        name, email = author[:-1].split(" <")
        git(d, "commit", "-q", "-m", msg, env={"GIT_AUTHOR_NAME": name, "GIT_AUTHOR_EMAIL": email,
                                               "GIT_COMMITTER_NAME": name, "GIT_COMMITTER_EMAIL": email,
                                               "GIT_AUTHOR_DATE": "2025-06-01T00:00:00", "GIT_COMMITTER_DATE":
                                               "2025-06-01T00:00:00"})

    def test_exclusions_and_a_locate_question(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "app"
            d.mkdir()
            git(d, "init", "-q", "-b", "main")
            self.commit(d, {"src/Cart.cs": "class Cart {\n int Total() { return 1; }\n}\n", "package-lock.json": "{}"},
                        "initial")
            self.commit(d, {"src/Cart.cs": "class Cart {\n  int Total() { return 1; }\n}\n"}, "reindent the cart")
            self.commit(d, {"package-lock.json": "{\"a\":1}"}, "bump lockfile only please")
            self.commit(d, {"src/Cart.cs": "class Cart {\n int Total() { return 2; }\n}\n"}, "bump", "jenkins <ci@x>")
            self.commit(d, {f"src/F{i}.cs": "class F {}\n" for i in range(31)}, "add many files")
            self.commit(d, {"src/Cart.cs": "class Cart {\n int Total() { return 3; }\n}\n",
                            "tests/CartTests.cs": "class CartTests {}\n"}, "Round cart totals per order line")
            rows = sample.changes(d)
            reasons = {}
            for c in rows:
                files = sample.changed_files(d, c["parent"], c["sha"])
                reasons[c["body"].splitlines()[0]] = sample.exclusion(d, c, files)
            self.assertEqual(reasons["reindent the cart"], "formatting only")
            self.assertEqual(reasons["bump lockfile only please"], "lockfile only")
            self.assertEqual(reasons["bump"], "bot")
            self.assertEqual(reasons["add many files"], "bulk (>30 files)")
            self.assertIsNone(reasons["Round cart totals per order line"])
            c = next(c for c in rows if c["body"].startswith("Round"))
            r = {"repo": "app", "sha": c["sha"], "parent": c["parent"], "merge": False,
                 "title": "Round cart totals per order line", "files": sample.changed_files(d, c["parent"], c["sha"])}
            q, lab = sample.locate_question(d, r, "A-locate-001", "history")
            self.assertEqual(q["index_state"], {"app": c["parent"]})
            self.assertEqual([i["item"] for i in lab["items"]], ["app/src/Cart.cs"])
            self.assertEqual(lab["tests"], [])  # the test file was added by the change, so no tool could name it
            self.assertEqual(validate_bank([q], [lab]), [])

    def test_activity_skips_bots_and_the_shallow_boundary(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t) / "app"
            d.mkdir()
            git(d, "init", "-q", "-b", "main")
            self.commit(d, {"a.cs": "1", "b.cs": "1", "c.cs": "1"}, "initial import")
            self.commit(d, {"b.cs": "2"}, "bump", "jenkins <ci@x>")
            self.commit(d, {"c.cs": "2"}, "fix c")
            self.assertEqual(sample.active_files(d, since="2025-01-01", until="2026-01-01"), {"c.cs"})


class Structural(unittest.TestCase):
    def test_candidates_by_stratum(self):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t)
            files = {
                "src/Svc.cs": "public class Svc {\n    public int Count(int a) { return a; }\n"
                              "    public override string ToString() => \"\";\n}\n",
                "src/ISvc.cs": "public interface ISvc { int Count(int a); }\n",
                "src/Api/OrdersController.cs": "public class OrdersController {\n    [HttpGet(\"{id}\")]\n"
                                               "    public IActionResult Get(int id) => Ok();\n}\n",
                "db/schema.sql": "CREATE TABLE `orders_2019_01` (id int);\nCREATE TABLE orders_2019_02 (id int);\n",
                "web/util.cfm": "<cfscript>\nfunction fmt(x) { return x; }\n</cfscript>\n",
                "WEB-INF/lucee/admin.cfc": "<cffunction name=\"vendored\">\n",
                "infra/main.tf": "resource \"aws_s3_bucket\" \"logs\" {\n}\n",
                "src/Gen.Designer.cs": "public class Gen { public void Hidden() {} }\n",
                "src/Generated/Model.cs": "public class Model { public void Hidden2() {} }\n",
                "src/Tool.cs": "// <auto-generated>\npublic class Tool { public void Hidden3() {} }\n",
            }
            for k, v in files.items():
                (d / k).parent.mkdir(parents=True, exist_ok=True)
                (d / k).write_text(v)

            def keys(stratum):
                return sorted(c["key"] for c in sample.structural_candidates(d, sorted(files), stratum, False, False))

            self.assertEqual(keys("cs-callers"), ["Count"])
            self.assertEqual(keys("di-implementations"), ["ISvc"])
            self.assertEqual(keys("http-cross-repo"), ["GET OrdersController.Get"])
            self.assertEqual(keys("sql-table-access"), ["schema.orders", "schema.orders"])  # db from the file name
            self.assertEqual(keys("cfml-callers"), ["fmt"])
            self.assertEqual(keys("terraform-deps"), ["aws_s3_bucket.logs"])


class Population(unittest.TestCase):
    def test_non_production_paths_and_split_tests(self):
        self.assertTrue(sample.non_production("src/App.TaskUtilities.TestJobs/Controllers/A.cs"))
        self.assertTrue(sample.non_production("host/Quickstart/Account/AccountController.cs"))
        self.assertTrue(sample.non_production("samples/app/Program.cs"))
        self.assertFalse(sample.non_production("src/Latest/Contest/Attestation.cs"))
        self.assertFalse(sample.non_production("src/App.AbTestApi/Service.cs"))

    def test_upstream_mirrors_by_license(self):
        with tempfile.TemporaryDirectory() as t:
            root, out = Path(t) / "repos", Path(t) / "out"
            for name, lic in (("ours", "Copyright (c) 2020 Acme Corp"), ("fork", "Copyright (c) 2016 Someone Else"),
                              ("Demo.Api", None), ("plain", None)):
                (root / name / ".git").mkdir(parents=True)
                if lic:
                    (root / name / "LICENSE").write_text(f"MIT License\n\n{lic}\n")
            out.mkdir()
            (out / "population.json").write_text('{"first_party": "acme", "exclude_repos": {"plain": "by hand"}}')
            ex = sample.population(root, out)
            self.assertEqual(sorted(ex), ["Demo.Api", "fork", "plain"])

    def test_cfml_functions_skip_javascript(self):
        page = ('<cffunction name="tagFn"></cffunction>\n<cfscript>\nfunction scriptFn(a) { return a; }\n'
                '</cfscript>\n<script>\nfunction jsFn() {}\n</script>\n')
        self.assertEqual([n for n, _ in sample.cfml_functions(page, ".cfm")], ["tagFn", "scriptFn"])
        comp = "component {\n  public string function go(x) { return x; }\n}\n"
        self.assertEqual([n for n, _ in sample.cfml_functions(comp, ".cfc")], ["go"])


class V2Rules(unittest.TestCase):
    def cands(self, files, stratum):
        with tempfile.TemporaryDirectory() as t:
            d = Path(t)
            for k, v in files.items():
                (d / k).parent.mkdir(parents=True, exist_ok=True)
                (d / k).write_text(v)
            return sample.structural_candidates(d, sorted(files), stratum, False, False)

    def test_bus_takes_messages_not_in_process_commands(self):
        files = {
            "a/Consumer.cs": "using MassTransit;\nclass C : IConsumer<OrderPlaced> {}\n",
            "a/Pub.cs": "using MassTransit;\nclass P { void Go() { _bus.Publish(new OrderShipped()); "
                        "_commandSender.Send(new RenameAccount()); } }\n",
            "a/Cqrs.cs": "class Q { void Go() { _sender.Send(new CreateThing()); _mediator.Publish(new Noted()); } }\n",
        }
        self.assertEqual(sorted(c["key"] for c in self.cands(files, "bus-pub-sub")), ["OrderPlaced", "OrderShipped"])

    def test_tables_name_their_database_and_mark_replicas(self):
        files = {
            "api/accounts.sql": "USE `accounts`;\nCREATE TABLE `cart_item` (id int);\n",
            "Warehouse/accounts.sql": "USE `accounts`;\nCREATE TABLE `cart_item` (id int);\n",
            "api/orders.sql": "CREATE TABLE orders.cart_item (id int);\n",
        }
        got = sorted((c["key"], c["replica"]) for c in self.cands(files, "sql-table-access"))
        self.assertEqual(got, [("accounts.cart_item", False), ("accounts.cart_item", True),
                               ("orders.cart_item", False)])

    def test_seen_subjects(self):
        with tempfile.TemporaryDirectory() as t:
            p = Path(t) / "seen.jsonl"
            p.write_text('{"kind": "symbol", "repo": "app", "value": "Save"}\n'
                         '{"kind": "file", "repo": "web", "value": "src/x.component.ts"}\n'
                         '{"kind": "symbol", "repo": "*", "value": "cart_item"}\n'
                         '{"kind": "repo", "repo": "svc", "value": "deploy"}\n')
            s = sample.Seen(p)
            self.assertTrue(s.subject("cs-callers", [{"repo": "app", "key": "Save", "file": "A.cs"}]))
            self.assertFalse(s.subject("cs-callers", [{"repo": "other", "key": "Save", "file": "A.cs"}]))
            self.assertTrue(s.subject("angular-ui-flow", [{"repo": "web", "key": "x", "file": "src/x.component.ts"}]))
            table = {"repo": "db", "key": "accounts.cart_item", "family": "cart_item", "file": "a"}
            self.assertTrue(s.subject("sql-table-access", [table]))
            self.assertTrue(s.subject("deploy-chain", [{"repo": "svc", "key": "svc", "file": ".gitlab-ci.yml"}]))
            self.assertFalse(s.subject("cs-callers", [{"repo": "svc", "key": "svc", "file": ".gitlab-ci.yml"}]))


class Agreement(unittest.TestCase):
    def test_item_f1_and_kappa(self):
        from score import agreement, cohen_kappa
        a = [label("Q1", ["app/a.cs:10", "app/b.cs:5"]), label("Q2", []), label("Q3", ["app/c.cs:1"])]
        b = [label("Q1", ["app/a.cs:11"]), label("Q2", []), label("Q3", ["app/d.cs:1"], completeness="lower_bound")]
        r = agreement(a, b)
        self.assertEqual(r["questions"], 3)
        f1 = {x["id"]: x["f1"] for x in r["per_question"]}
        self.assertAlmostEqual(f1["Q1"], 2 / 3)
        self.assertEqual((f1["Q2"], f1["Q3"]), (1.0, 0.0))
        self.assertAlmostEqual(r["f1_pooled"], 2 * 1 / (3 + 2))
        self.assertAlmostEqual(cohen_kappa([("x", "x"), ("y", "y")]), 1.0)
        self.assertAlmostEqual(cohen_kappa([("x", "y"), ("y", "x")]), -1.0)


class Score(unittest.TestCase):
    def test_exhaustive_counts_precision_and_recall(self):
        lab = label(items=["app/a.cs:10", "app/b.cs:20"], tests=["app/tests/T.cs:5"])
        r = score_one(lab, ["app/a.cs:11", "app/c.cs:1", "app/tests/T.cs:5", "app/a.cs:12"], ["app"])
        self.assertEqual((r["tp"], r["fn"], r["fp"]), (1, 1, 1))
        self.assertAlmostEqual(r["precision"], 0.5)
        self.assertAlmostEqual(r["recall"], 0.5)
        self.assertEqual(r["missed"], ["app/b.cs:20"])

    def test_lower_bound_scores_recall_only(self):
        lab = label(items=["app/a.cs:10"], completeness="lower_bound")
        r = score_one(lab, ["a.cs:10", "app/z.cs:3"], ["app"])  # repo prefix added for a one-repo question
        self.assertEqual((r["tp"], r["fp"], r["precision"], r["recall"]), (1, None, None, 1.0))
        cross = label(items=["other/x.cfm"], match="file")  # a cross-repo answer to a question about repo "app"
        self.assertEqual(score_one(cross, ["other/x.cfm"], ["app"])["tp"], 1)

    def test_match_rules(self):
        self.assertEqual(score_one(label(items=["app/a.cs"], match="file"), ["app/a.cs:99"])["tp"], 1)
        self.assertEqual(score_one(label(items=["app/a.cs:10"], match="file_line"), ["app/a.cs:11"])["tp"], 0)
        self.assertEqual(score_one(label(items=["GET /api/orders"], match="ci"), ["get /API/orders"])["tp"], 1)
        self.assertEqual(score_one(label(items=["aws_s3_bucket.logs"], match="symbol"), ["aws_s3_bucket.Logs"])["tp"],
                         0)

    def test_empty_label_is_a_real_answer(self):
        nothing = score_one(label(items=[]), [])
        self.assertEqual((nothing["recall"], nothing["precision"], nothing["fp"]), (1.0, 1.0, 0))
        something = score_one(label(items=[]), ["app/x.cs:1"])
        self.assertEqual((something["recall"], something["precision"], something["fp"]), (0.0, 0.0, 1))

    def test_routes_normalize_slashes_case_and_params(self):
        lab = label(items=["GET /api/Orders/{id}/"], match="ci")
        self.assertEqual(score_one(lab, ["get api/orders/:orderId"])["tp"], 1)
        self.assertEqual(score_one(lab, ["POST /api/orders/{id}"])["tp"], 0)
        self.assertEqual(score_one(lab, ["GET /api/orders"])["tp"], 0)

    def test_optional_items_cost_nothing_when_missed(self):
        lab = label(items=["app/a.cs:1", "app/b.cs:1"])
        lab["items"][1]["optional"] = True
        r = score_one(lab, ["app/a.cs:1"])
        self.assertEqual((r["tp"], r["fn"], r["recall"], r["precision"]), (1, 0, 1.0, 1.0))
        r = score_one(lab, ["app/a.cs:1", "app/b.cs:1"])
        self.assertEqual((r["tp"], r["fp"], r["recall"]), (2, 0, 1.0))

    def test_undecided_items_are_neither_hits_nor_false_positives(self):
        # Callers through an external SDK: the labeller can't tell from source whether they reach the route.
        lab = label(items=["web/src/a.ts"], match="file")
        lab["undecided"] = [{"item": "web/src/sdk-user.ts", "evidence": "calls an SDK method whose route isn't in the cohort"}]
        r = score_one(lab, ["web/src/a.ts", "web/src/sdk-user.ts:12", "web/src/other.ts"])
        self.assertEqual((r["tp"], r["fp"], r["recall"]), (1, 1, 1.0))
        self.assertAlmostEqual(r["precision"], 0.5)
        self.assertEqual((r["undecided_returned"], r["extra"]), (["web/src/sdk-user.ts:12"], ["web/src/other.ts"]))
        # An empty label stays a correct empty answer when only undecided calls are named.
        empty = label(items=[], match="file")
        empty["undecided"] = lab["undecided"]
        r = score_one(empty, ["web/src/sdk-user.ts"])
        self.assertEqual((r["recall"], r["precision"], r["fp"]), (1.0, 1.0, 0))
        # Without the list, the same call is a false positive, as before.
        self.assertEqual(score_one(label(items=[], match="file"), ["web/src/sdk-user.ts"])["fp"], 1)

    def test_dispatch_items_can_be_left_out(self):
        lab = label(items=["app/a.cs:1", "app/b.cs:1"])
        lab["items"][1]["via"] = "dispatch"
        self.assertAlmostEqual(score_one(lab, ["app/a.cs:1"])["recall"], 0.5)
        r = score_one(lab, ["app/a.cs:1", "app/b.cs:1"], skip_via=True)
        self.assertEqual((r["recall"], r["fp"]), (1.0, 0))

    def test_negatives_are_false_positives_even_on_lower_bounds(self):
        lab = label(items=["app/a.cs:1"], completeness="lower_bound")
        lab["negatives"] = [{"item": "other/a.cs:1", "evidence": "same name, other class"}]
        r = score_one(lab, ["app/a.cs:1", "other/a.cs:1", "app/maybe.cs:3"], ["app"])
        self.assertEqual((r["tp"], r["fp"], r["negatives_returned"]), (1, 1, ["other/a.cs:1"]))
        self.assertAlmostEqual(r["precision"], 0.5)

    def test_unsupported_is_not_wrong(self):
        labels = [label("Q1", ["app/a.cs:1"]), label("Q2", ["app/b.cs:1"]), label("Q3", ["app/c.cs:1"])]
        answers = [{"id": "Q1", "status": "answered", "items": ["app/a.cs:1"]}, {"id": "Q2", "status": "unsupported"}]
        s = score(labels, answers, [question("Q1"), question("Q2"), question("Q3")])["summary"]
        e = s["by_completeness"]["exhaustive"]
        self.assertEqual((e["answered"], e["unsupported"], e["missing"]), (1, 1, 1))
        self.assertEqual((e["tp"], e["fn"], e["recall_pooled"], e["precision_pooled"]), (1, 0, 1.0, 1.0))
        self.assertEqual(s["by_stratum"]["cs-callers"]["questions"], 3)
        labels[2]["excluded"] = True  # an excluded question is not scored at all
        s = score(labels, answers, [question("Q1"), question("Q2"), question("Q3")])["summary"]
        self.assertEqual(s["by_completeness"]["exhaustive"]["missing"], 0)

    def test_weighted_headline_uses_corpus_share_not_question_counts(self):
        act = {"subset": "active"}
        qs = [question("A1", meta=act), question("A2", meta=act), question("A3", meta=act),
              question("B1", stratum="terraform-deps", meta=act), question("U1", meta={"subset": "uniform"})]
        labels = [label(q["id"], ["app/a.cs:1"]) for q in qs]
        answers = [{"id": i, "items": ["app/a.cs:1"]} for i in ("A1", "A2", "A3")] + [{"id": "B1", "items": []}]
        w = score(labels, answers, qs, {"cs-callers": 0.9, "terraform-deps": 0.1})["summary"]["weighted_structural"]
        self.assertAlmostEqual(w["recall"], 0.9)


if __name__ == "__main__":
    unittest.main()
