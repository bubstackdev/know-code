#!/usr/bin/env python3
"""What KnowCode's MCP tools cost a session's context before any call: the tokens of the tool schemas and the server
instructions, counted by the Anthropic token-counting endpoint (`/v1/messages/count_tokens`, which is free and runs
no model).

Usage: scripts/eval/schema_cost.py --bin BIN [--model MODEL] [--out FILE.md] [--json FILE.json]

The script starts `BIN serve --tools PROFILE` for each profile, in a scratch cache, config and data directory of its
own, and reads what the server sends (`initialize` instructions, `tools/list`). It never queries a repository.

Every figure is a difference between two counts of the same request, with and without KnowCode's part, so the
client's own system prompt and tools are left out:

- the base request has a short system prompt, one user message and one stand-in client tool (so the fixed preamble the
  API adds whenever tools are present is in the base, not charged to KnowCode);
- tool search off: every advertised tool's schema is sent, plus the instructions;
- tool search on: as Claude Code defers MCP tools, only the always-loaded tools (`_meta` `anthropic/alwaysLoad`) are
  sent as schemas, and the deferred tools are named in one message, one `mcp__<server>__<tool>` per line; plus the
  instructions. Loading a deferred tool later costs its own schema (the per-tool table);
- the instructions on their own, and each tool's schema on its own.

The key is read from ANTHROPIC_API_KEY and is never printed or written.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))  # scripts/: kc_env
import kc_env  # noqa: E402

ENDPOINT = "https://api.anthropic.com/v1/messages/count_tokens"
SERVER = "knowcode"
PROFILES = ["one", "all", "core"]
BASE_SYSTEM = "You are a coding agent working in a repository."
BASE_USER = "Which files call the HTTP endpoint for GET /orders?"
# A client tool, so the tool-use preamble is part of every count, including the base.
CLIENT_TOOL = {
    "name": "Read",
    "description": "Reads a file.",
    "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]},
}
# Always-loaded sets to compare under tool search (profile `all`), besides none and the server's current set.
CANDIDATES = [["kc_context"], ["kc_context", "kc_locate"], ["kc_context", "kc_refs", "kc_locate"],
              ["kc_context", "kc_refs", "kc_impact", "kc_locate", "kc_find"]]
DEFERRED_INTRO = "The following deferred tools are available through tool search; load one's schema before calling it:"


def server_view(bin_, profile):
    """(instructions, tools) as `BIN serve --tools profile` advertises them, from a scratch environment."""
    with tempfile.TemporaryDirectory(prefix="kc-schema-cost-") as tmp:
        env = kc_env.knowcode_env(tmp)
        env.pop("KNOWCODE_TOOLS", None)
        msgs = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize",
             "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "schema_cost", "version": "1"}}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        ]
        stdin = "".join(json.dumps(m) + "\n" for m in msgs)
        out = subprocess.run([str(bin_), "serve", "--tools", profile], input=stdin, capture_output=True, text=True,
                             env=env, cwd=tmp, timeout=60)
    replies = {m.get("id"): m for m in map(json.loads, filter(None, out.stdout.splitlines()))}
    if 1 not in replies or 2 not in replies:
        sys.exit(f"serve --tools {profile} answered no tools/list: {out.stderr.strip()[-300:]}")
    return replies[1]["result"].get("instructions", ""), replies[2]["result"]["tools"]


def api_tool(tool):
    """The tool as a client sends it to the model: prefixed name, description, input schema."""
    return {"name": f"mcp__{SERVER}__{tool['name']}", "description": tool.get("description", ""), "input_schema": tool["inputSchema"]}


def always_loaded(tool):
    return (tool.get("_meta") or {}).get("anthropic/alwaysLoad") is True


def deferred_listing(names):
    return DEFERRED_INTRO + "\n" + "\n".join(f"mcp__{SERVER}__{n}" for n in names)


def request(model, tools=(), instructions="", listing=""):
    """A count_tokens request: the base, plus KnowCode's schemas, instructions and deferred listing."""
    system = BASE_SYSTEM + (f"\n\n# MCP Server Instructions\n\n## {SERVER}\n{instructions}" if instructions else "")
    content = (listing + "\n\n" if listing else "") + BASE_USER
    return {"model": model, "system": system, "messages": [{"role": "user", "content": content}],
            "tools": [CLIENT_TOOL, *tools]}


def count(body, key):
    req = urllib.request.Request(ENDPOINT, data=json.dumps(body).encode(), method="POST", headers={
        "x-api-key": key, "anthropic-version": "2023-06-01", "content-type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.load(r)["input_tokens"]
    except urllib.error.HTTPError as e:
        sys.exit(f"count_tokens: HTTP {e.code}: {e.read().decode(errors='replace')[:300]}")


def search_on(model, tools, loaded_names, instructions, key, base):
    """Tool search on, with `loaded_names` loaded up front and the rest of `tools` named in the deferred listing."""
    loaded = [api_tool(t) for t in tools if t["name"] in loaded_names]
    deferred = [t["name"] for t in tools if t["name"] not in loaded_names]
    return count(request(model, loaded, instructions, deferred_listing(deferred) if deferred else ""), key) - base


def measure(bin_, model, key):
    base = count(request(model), key)
    rows, per_tool = [], {}
    views = {p: server_view(bin_, p) for p in PROFILES}
    instructions = views["all"][0]
    for profile, (instr, tools) in views.items():
        loaded = {t["name"] for t in tools if always_loaded(t)}
        rows.append({"profile": profile, "tools": len(tools), "always_loaded": len(loaded), "deferred": len(tools) - len(loaded),
                     "schema_bytes": len(json.dumps(tools)),
                     "search_off": count(request(model, [api_tool(t) for t in tools], instr), key) - base,
                     "search_on": search_on(model, tools, loaded, instr, key, base)})
    tools = views["all"][1]
    for t in tools:
        per_tool[t["name"]] = {"tokens": count(request(model, [api_tool(t)]), key) - base,
                               "bytes": len(json.dumps(t)), "always_loaded": always_loaded(t)}
    current = [t["name"] for t in tools if always_loaded(t)]
    candidates = [("none", [])] + [(", ".join(c), c) for c in CANDIDATES] + [("current: " + ", ".join(current), current)]
    sets = [{"set": label, "tools": len(names), "search_on": search_on(model, tools, set(names), instructions, key, base)}
            for label, names in candidates]
    return {"model": model, "base": base, "instructions": count(request(model, instructions=instructions), key) - base,
            "always_loaded": current,
            "always_loaded_schemas": count(request(model, [api_tool(t) for t in tools if always_loaded(t)]), key) - base,
            "profiles": rows, "per_tool": per_tool, "candidate_sets": sets}


def table(m):
    out = [f"Model `{m['model']}`. Tokens are added to a base request of {m['base']} tokens (short system prompt, one "
           "user message, one client tool).", "",
           "| Profile | Tools | Always loaded | Deferred | Schema bytes | Tool search off | Tool search on |",
           "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for r in m["profiles"]:
        out.append(f"| `{r['profile']}` | {r['tools']} | {r['always_loaded']} | {r['deferred']} | {r['schema_bytes']:,} | "
                   f"{r['search_off']:,} | {r['search_on']:,} |")
    out += ["", f"Of which: server instructions {m['instructions']:,} tokens; the always-loaded tools' schemas "
            f"({', '.join('`' + n + '`' for n in m['always_loaded'])}) {m['always_loaded_schemas']:,} tokens.", "",
            "| Tool | Always loaded | Schema bytes | Tokens on its own |", "| --- | --- | ---: | ---: |"]
    for name, v in sorted(m["per_tool"].items(), key=lambda kv: -kv[1]["tokens"]):
        out.append(f"| `{name}` | {'yes' if v['always_loaded'] else ''} | {v['bytes']:,} | {v['tokens']:,} |")
    out += ["", "Always-loaded sets, profile `all`, tool search on (instructions and deferred listing included):", "",
            "| Loaded up front | Tools | Tokens |", "| --- | ---: | ---: |"]
    for r in m["candidate_sets"]:
        out.append(f"| {r['set']} | {r['tools']} | {r['search_on']:,} |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--bin", required=True, help="the knowcode binary")
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--out", help="write the markdown table here")
    ap.add_argument("--json", help="write the raw counts here")
    a = ap.parse_args()
    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        sys.exit("ANTHROPIC_API_KEY is not set")
    m = measure(Path(a.bin).resolve(), a.model, key)
    text = table(m)
    print(text, end="")
    if a.out:
        Path(a.out).write_text(text)
    if a.json:
        Path(a.json).write_text(json.dumps(m, indent=2) + "\n")


if __name__ == "__main__":
    main()
