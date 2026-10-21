#!/usr/bin/env python3
"""Unprompted-use tuning harness: headless Claude Code sessions on public repos, counting KnowCode and plain-tool calls.

This is a small tuning loop for the server instructions, tool descriptions and skill descriptions. It is separate
from the frozen public study (eval/public-v1): its questions (adoption-questions.jsonl) are not the study's, and
nothing here reads or writes the study's files. Numbers from it are a small sample for tuning, not a result.

Each session runs `claude --bare --print QUESTION` in a clone of a public repo, with KnowCode available but not named.
--repos holds one clone per question `repo`, by that name, checked out at the question's `commit` (from its `url`):

- `mcp`: KnowCode's MCP server only (`--mcp-config`): its instructions and tool descriptions.
- `plugin`: the same server plus the skills of the Claude Code plugin in integrations/claude-code (`--plugin-dir`).

Under --bare the client runs no hooks and offers no tool search: every MCP tool's schema is in context from the
start, so neither the Bash hint hook nor the always-loaded mark plays a part. What this measures is whether the
instructions and descriptions lead the agent to pick KnowCode over its own Bash and Read. --full-client drops --bare:
the client then runs the plugin's hooks and defers MCP tools other than the always-loaded ones behind ToolSearch, as
in an interactive session, still with the isolated configuration below and the API key (its init message reports
`apiKeySource`). A full-client session costs more, mostly for the client's longer system prompt.

Every session gets its own HOME, CLAUDE_CONFIG_DIR and KNOWCODE_* directories under --work; the KnowCode index is
built once per repo under --work beforehand (never in the repo). The API key comes from ANTHROPIC_API_KEY in the
environment and is passed to the client only; it is never written or printed. Spend is read from each session's
result message, appended to --work/ledger.tsv, and no session starts once the ledger total reaches --budget-usd.

  adoption_tune.py run --repos DIR --knowcode BIN --work DIR --label NAME [--config mcp --config plugin] ...
  adoption_tune.py summary --work DIR [--label NAME]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from clean_machine import is_knowcode_tool  # noqa: E402

QUESTIONS = HERE / "adoption-questions.jsonl"
PLUGIN = HERE.parent.parent / "integrations" / "claude-code"
CONFIGS = ("mcp", "plugin")
# Read-only shell programs the agent may run, as in the study's plain arm.
PROGRAMS = ("grep", "rg", "find", "cat", "sed", "head", "tail", "wc", "ls", "sort", "uniq", "xargs")


def load_questions(path: Path) -> list[dict]:
    questions = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    for q in questions:
        if not all(isinstance(q.get(k), str) and q[k] for k in ("id", "repo", "question")):
            raise ValueError(f"question needs id, repo and question: {q}")
    if len({q["id"] for q in questions}) != len(questions):
        raise ValueError("duplicate question id")
    return questions


def ledger_total(work: Path) -> float:
    ledger = work / "ledger.tsv"
    if not ledger.exists():
        return 0.0
    total = 0.0
    for line in ledger.read_text().splitlines()[1:]:
        fields = line.split("\t")
        if len(fields) >= 5:
            total += float(fields[4] or 0)
    return total


def record_spend(work: Path, label: str, config: str, qid: str, cost: float) -> float:
    ledger = work / "ledger.tsv"
    if not ledger.exists():
        ledger.write_text("label\tconfig\tquestion\trun\tcost_usd\tcumulative_usd\n")
    total = ledger_total(work) + cost
    runs = sum(1 for line in ledger.read_text().splitlines()[1:] if line.split("\t")[:3] == [label, config, qid])
    with ledger.open("a") as stream:
        stream.write(f"{label}\t{config}\t{qid}\t{runs + 1}\t{cost:.4f}\t{total:.4f}\n")
    return total


def session_argv(client: str, question: str, config: str, *, model: str, effort: str, max_usd: float,
                 settings: Path, mcp: Path, knowcode: Path, bare: bool = True) -> list[str]:
    allowed = [f"Bash({p} *)" for p in PROGRAMS] + ["Read"]
    argv = [client, *(["--bare"] if bare else []), "--print", question, "--model", model, "--effort", effort,
            "--output-format", "stream-json", "--verbose", "--no-session-persistence",
            "--setting-sources", "", "--settings", str(settings), "--permission-mode", "dontAsk",
            "--max-budget-usd", f"{max_usd:.2f}", "--strict-mcp-config"]
    if config == "mcp":
        argv += ["--tools", "Bash,Read", "--mcp-config", str(mcp), "--allowedTools", *allowed, "mcp__knowcode"]
    elif config == "plugin":
        # The plugin's skills, with its server from --mcp-config: under --bare the plugin's own .mcp.json isn't started.
        argv += ["--tools", "Bash,Read,Skill", "--plugin-dir", str(PLUGIN), "--mcp-config", str(mcp),
                 "--allowedTools", *allowed, "Skill", "mcp__knowcode"]
    else:
        raise ValueError(f"unknown config {config}")
    if not bare:
        # Without --bare the client defers MCP tools behind ToolSearch, as in an interactive session.
        i = argv.index("--tools") + 1
        argv[i] += ",ToolSearch"
        argv.append("--allowedTools")
        argv.append("ToolSearch")
    # Notes write local state; tuning sessions only read.
    argv += ["--disallowedTools", "mcp__knowcode__kc_note_add"]
    return argv


def session_env(base: Path, knowcode: Path, cache: Path, client: str) -> dict:
    home = base / "home"
    for d in (home, base / "claude-config", base / "kc-config", base / "kc-data", base / "tmp"):
        d.mkdir(parents=True, exist_ok=True)
    path = os.pathsep.join([str(knowcode.parent), str(Path(client).parent), "/usr/bin", "/bin", "/usr/sbin", "/sbin"])
    env = {"HOME": str(home), "PATH": path, "LC_ALL": "C", "SHELL": "/bin/bash", "TMPDIR": str(base / "tmp"),
           "CLAUDE_CONFIG_DIR": str(base / "claude-config"), "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "KNOWCODE_CACHE_DIR": str(cache), "KNOWCODE_CONFIG_DIR": str(base / "kc-config"),
           "KNOWCODE_DATA_DIR": str(base / "kc-data"), "KNOWCODE_UPDATE_CHECK": "0"}
    env["ANTHROPIC_API_KEY"] = os.environ["ANTHROPIC_API_KEY"]
    return env


def parse_stream(path: Path) -> dict:
    """Tool calls and spend from a `--output-format stream-json` transcript."""
    out = {"kc_calls": [], "plain_calls": {}, "meta_calls": {}, "cost_usd": 0.0, "turns": None, "result": None,
           "is_error": None, "tools_offered": None}
    for raw in path.read_text(errors="replace").splitlines():
        try:
            entry = json.loads(raw)
        except ValueError:
            continue
        if not isinstance(entry, dict):
            continue
        if entry.get("type") == "system" and entry.get("subtype") == "init":
            out["tools_offered"] = sorted(t for t in entry.get("tools", []) if isinstance(t, str))
        elif entry.get("type") == "assistant":
            content = (entry.get("message") or {}).get("content")
            for block in content if isinstance(content, list) else []:
                if not isinstance(block, dict) or block.get("type") != "tool_use":
                    continue
                name, inp = block.get("name", "?"), block.get("input")
                if is_knowcode_tool(name, inp):
                    out["kc_calls"].append(name if name != "Bash" else "Bash: knowcode")
                elif name in ("ToolSearch", "Skill"):
                    detail = (inp or {}).get("query") or (inp or {}).get("skill") or ""
                    out["meta_calls"].setdefault(name, []).append(str(detail)[:120])
                else:
                    out["plain_calls"][name] = out["plain_calls"].get(name, 0) + 1
        elif entry.get("type") == "result":
            out["cost_usd"] = float(entry.get("total_cost_usd") or 0.0)
            out["turns"] = entry.get("num_turns")
            out["is_error"] = entry.get("is_error")
            out["result"] = str(entry.get("result") or "")[:2000]
    out["kc_call_count"] = len(out["kc_calls"])
    out["plain_call_count"] = sum(out["plain_calls"].values())
    return out


def index_repos(knowcode: Path, repos: Path, names: set[str], cache: Path, work: Path) -> None:
    env = {"HOME": str(work / "index-home"), "PATH": "/usr/bin:/bin", "KNOWCODE_CACHE_DIR": str(cache),
           "KNOWCODE_CONFIG_DIR": str(work / "index-config"), "KNOWCODE_DATA_DIR": str(work / "index-data"),
           "KNOWCODE_UPDATE_CHECK": "0"}
    for name in sorted(names):
        subprocess.run([str(knowcode), "index", str(repos / name)], env=env, check=True, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL)


def run(args) -> int:
    if not os.environ.get("ANTHROPIC_API_KEY"):
        print("ANTHROPIC_API_KEY is not set; no session started", file=sys.stderr)
        return 2
    work, repos, knowcode = Path(args.work).resolve(), Path(args.repos).resolve(), Path(args.knowcode).resolve()
    client = shutil.which(args.client) or args.client
    questions = [q for q in load_questions(Path(args.questions)) if not args.only or q["id"] in args.only]
    work.mkdir(parents=True, exist_ok=True)
    cache = work / "kc-cache"
    if not args.no_index:
        index_repos(knowcode, repos, {q["repo"] for q in questions}, cache, work)
    for config in args.config or list(CONFIGS):
        for q in questions:
            for n in range(args.repeat):
                spent = ledger_total(work)
                if spent + args.session_usd > args.budget_usd:
                    print(f"budget: {spent:.2f} spent of {args.budget_usd:.2f}; stopping", file=sys.stderr)
                    return 3
                base = work / "sessions" / args.label / config / f"{q['id']}-{n + 1}"
                if (base / "stream.jsonl").exists():
                    continue
                base.mkdir(parents=True, exist_ok=True)
                (base / "settings.json").write_text("{}\n")
                (base / "mcp.json").write_text(json.dumps({"mcpServers": {"knowcode": {
                    "type": "stdio", "command": str(knowcode), "args": ["serve"],
                    "env": {"KNOWCODE_CACHE_DIR": str(cache), "KNOWCODE_UPDATE_CHECK": "0", "ANTHROPIC_API_KEY": ""}}}},
                    indent=2) + "\n")
                argv = session_argv(client, q["question"], config, model=args.model, effort=args.effort,
                                    max_usd=args.session_usd, settings=base / "settings.json", mcp=base / "mcp.json",
                                    knowcode=knowcode, bare=not args.full_client)
                with (base / "stream.jsonl").open("w") as stream:
                    code = subprocess.run(argv, cwd=repos / q["repo"], env=session_env(base, knowcode, cache, client),
                                          stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.DEVNULL,
                                          timeout=args.timeout).returncode
                parsed = parse_stream(base / "stream.jsonl")
                parsed.update({"label": args.label, "config": config, "question": q["id"], "exit": code})
                (base / "session.json").write_text(json.dumps(parsed, indent=2) + "\n")
                total = record_spend(work, args.label, config, q["id"], parsed["cost_usd"])
                print(f"{args.label} {config} {q['id']}#{n + 1}: kc={parsed['kc_call_count']} "
                      f"plain={parsed['plain_call_count']} cost={parsed['cost_usd']:.3f} total={total:.2f}")
    return 0


def summarize(work: Path, label: str | None = None) -> dict:
    """Per label and config: sessions, sessions that used KnowCode, and KnowCode and plain-tool call counts."""
    groups: dict = {}
    for path in sorted((work / "sessions").glob("*/*/*/session.json")):
        s = json.loads(path.read_text())
        if label and s["label"] != label:
            continue
        g = groups.setdefault(f"{s['label']}/{s['config']}", {
            "sessions": 0, "sessions_using_knowcode": 0, "kc_calls": 0, "plain_calls": 0, "cost_usd": 0.0,
            "kc_tools": {}, "per_question": {}})
        g["sessions"] += 1
        g["sessions_using_knowcode"] += s["kc_call_count"] > 0
        g["kc_calls"] += s["kc_call_count"]
        g["plain_calls"] += s["plain_call_count"]
        g["cost_usd"] = round(g["cost_usd"] + s["cost_usd"], 4)
        for t in s["kc_calls"]:
            short = t.rsplit("__", 1)[-1]
            g["kc_tools"][short] = g["kc_tools"].get(short, 0) + 1
        g["per_question"].setdefault(s["question"], []).append([s["kc_call_count"], s["plain_call_count"]])
    return {"format": 1, "groups": groups, "ledger_total_usd": round(ledger_total(work), 4)}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("run")
    p.add_argument("--repos", required=True, help="directory holding one clone per question repo, by name")
    p.add_argument("--knowcode", required=True, help="the knowcode binary to serve and index with")
    p.add_argument("--work", required=True, help="session directories, index cache and ledger")
    p.add_argument("--label", required=True, help="name of this round, e.g. before or after-1")
    p.add_argument("--config", action="append", choices=CONFIGS)
    p.add_argument("--questions", default=str(QUESTIONS))
    p.add_argument("--only", action="append", help="question id to run (repeatable)")
    p.add_argument("--repeat", type=int, default=1)
    p.add_argument("--model", default="claude-opus-5-5")
    p.add_argument("--effort", default="medium")
    p.add_argument("--client", default="claude")
    p.add_argument("--budget-usd", type=float, required=True, help="stop before the ledger total would pass this")
    p.add_argument("--session-usd", type=float, default=1.0, help="per-session cap (--max-budget-usd)")
    p.add_argument("--timeout", type=int, default=900)
    p.add_argument("--full-client", action="store_true", help="without --bare: hooks and tool search as in an "
                   "interactive session")
    p.add_argument("--no-index", action="store_true", help="reuse the index already in --work")
    p = sub.add_parser("summary")
    p.add_argument("--work", required=True)
    p.add_argument("--label")
    args = parser.parse_args(argv)
    if args.command == "run":
        return run(args)
    print(json.dumps(summarize(Path(args.work), args.label), indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
