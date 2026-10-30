#!/usr/bin/env python3
"""Fails when the docs and the binary disagree: a doc names a command, flag, config key, environment variable or MCP
tool that doesn't exist, or a reference page leaves out one that does.

Read from the binary: commands and flags (`knowcode --help`, `knowcode <command> --help`, recursively), MCP tools and
their parameters (`tools/list` from `knowcode serve`, and each tool's `kc_help` topic), and config keys (`knowcode config list`). Read from the source:
environment variables (`"KNOWCODE_…"` in `crates/*/src`) and the answer contract's closed sets (`Reason`,
`Mechanism`, `Assumption`, `Code` in `crates/kc-store/src/answer.rs`).

Checked docs: README.md, CHANGELOG.md, docs/*.md, integrations/claude-code/README.md and its skills.

- Named but missing: `knowcode <command> [<subcommand>]` and each `--flag` after it on the same line, `kc_*` tools
  and `kc_help` topics, config keys (`cache.dir`), `KNOWCODE_*` variables, and an MCP tool's `param:` in a call
  written as `kc_tool(param: …)`.
- Missing from the page that documents them: every command and every flag in docs/usage.md, every MCP tool in
  docs/usage.md, every config key and user-facing variable in docs/configuration.md, every reason, mechanism,
  assumption and code in docs/answers.md.

Usage: scripts/check-docs.py [path/to/knowcode]   (default target/debug/knowcode; exit 1 listing each problem)
"""

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))  # kc_env, beside this script
import kc_env  # noqa: E402


ROOT = Path(__file__).resolve().parent.parent
CRATES = {"kc_store", "kc_index", "kc_extract"}
COMMON_FLAGS = {"--repo", "--no-sync", "--json", "--provenance", "--help", "--version"}
# Variables that are not user settings: build stamps, test hooks and internal handshakes.
INTERNAL_ENV = {"KNOWCODE_BUILD", "KNOWCODE_KEEP_RESUME", "KNOWCODE_INSERT_REVERSED", "KNOWCODE_BOUND_REPOS"}
INTERNAL_ENV_PREFIXES = ("KNOWCODE_TEST_",)
# Variables of the cargo-dist installer, which self-update passes on or clears; documented by example only.
INSTALLER_ENV = {"KNOWCODE_INSTALL_DIR", "KNOWCODE_NO_MODIFY_PATH", "KNOWCODE_UNMANAGED_INSTALL", "KNOWCODE_DOWNLOAD_URL",
                 "KNOWCODE_GITHUB_TOKEN", "KNOWCODE_INSTALLER_GITHUB_BASE_URL", "KNOWCODE_INSTALLER_GHE_BASE_URL",
                 "KNOWCODE_DISABLE_UPDATE"}
# Variables read by scripts rather than the binary.
SCRIPT_ENV = {"KNOWCODE_EVAL_DIR", "KNOWCODE_GITLAB_HOST", "KNOWCODE_GITLAB_CORPUS", "KNOWCODE_TEST_MACOS_SANDBOX"}


def docs():
    out = [ROOT / "README.md", ROOT / "CHANGELOG.md", *sorted((ROOT / "docs").glob("*.md"))]
    plugin = ROOT / "integrations" / "claude-code"
    out += [plugin / "README.md", *sorted(plugin.glob("skills/*/SKILL.md"))]
    return [p for p in out if p.exists()]


def run(binary, *args, env=None):
    return subprocess.run([binary, *args], capture_output=True, text=True, env=env, timeout=60).stdout


def section(text, title):
    """The lines of a clap help section (`Commands:`, `Options:`)."""
    lines, on = [], False
    for line in text.splitlines():
        if line.rstrip() == title:
            on = True
        elif on and line and not line.startswith(" "):
            break
        elif on:
            lines.append(line)
    return lines


def cli(binary, env):
    """{command path: set of flags}, commands as `index`, `note add`."""
    out = {}

    def walk(path):
        text = run(binary, *path, "--help", env=env)
        flags = set()
        for line in section(text, "Options:"):
            m = re.match(r"\s+(?:-\w, )?(--[a-z][\w-]*)", line)
            if m:
                flags.add(m.group(1))
            m = re.search(r"\[alias(?:es)?: ([^\]]+)\]", line)
            if m:
                flags.update(re.findall(r"--[a-z][\w-]*", m.group(1)))
        if path:
            out[" ".join(path)] = flags
        for line in section(text, "Commands:"):
            m = re.match(r"\s+([a-z][\w-]*)\s", line + " ")
            if m and m.group(1) != "help":
                walk([*path, m.group(1)])

    walk([])
    return out


def mcp(binary, env, cwd):
    """{tool: set of parameters}, and the help topics. A tool's parameters are the ones its help topic lists: its
    schema advertises only the common ones, and takes every one the topic lists."""
    p = subprocess.Popen([binary, "serve", "--tools", "all"], cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True)

    def send(msgs, want):
        for m in msgs:
            p.stdin.write(json.dumps(m) + "\n")
        p.stdin.flush()
        got = {}
        while len(got) < want:
            line = p.stdout.readline()
            if not line:
                break
            m = json.loads(line)
            if "id" in m:
                got[m["id"]] = m
        return got

    got = send([
        {"jsonrpc": "2.0", "id": 0, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "check-docs", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "kc_help", "arguments": {}}},
    ], 3)
    tools = {t["name"]: set(t["inputSchema"].get("properties", {})) for t in got[1]["result"]["tools"]}
    names = sorted(tools)
    helps = send([{"jsonrpc": "2.0", "id": 10 + i, "method": "tools/call", "params": {"name": "kc_help", "arguments": {"topic": n}}}
                  for i, n in enumerate(names)], len(names))
    for i, n in enumerate(names):
        text = "".join(c.get("text", "") for c in helps.get(10 + i, {}).get("result", {}).get("content", []))
        section = text.split("\nParameters:\n", 1)[1].split("\n\n", 1)[0] if "\nParameters:\n" in text else ""
        tools[n] |= set(re.findall(r"^- `([^`]+)` \(", section, re.M))
    p.stdin.close()
    p.wait(timeout=30)
    index = "".join(c.get("text", "") for c in got[2]["result"]["content"])
    topics = set(re.findall(r"^- `([^`]+)`", index, re.M))
    return tools, topics


def config_keys(binary, env):
    return {m.group(1) for m in re.finditer(r"^([a-z_]+(?:\.[a-z_]+)+) = ", run(binary, "config", "list", env=env), re.M)}


def env_vars():
    names = set()
    for f in (ROOT / "crates").glob("*/src/**/*.rs"):
        names.update(re.findall(r'"(KNOWCODE_[A-Z_]+)"', f.read_text()))
    for f in (ROOT / "integrations").glob("**/*.sh"):
        names.update(re.findall(r"\b(KNOWCODE_[A-Z_]+)\b", f.read_text()))
    return names


def contract_sets():
    """{enum name: set of snake_case members} from answer.rs."""
    text = (ROOT / "crates" / "kc-store" / "src" / "answer.rs").read_text()
    out = {}
    for name in ("Reason", "Mechanism", "Assumption", "Code"):
        body = re.search(r"pub enum " + name + r" \{(.*?)\n\}", text, re.S).group(1)
        members = re.findall(r"^\s{4}([A-Z]\w*),", body, re.M)
        out[name] = {re.sub(r"(?<!^)([A-Z])", r"_\1", m).lower() for m in members}
    return out


def check(binary):
    problems = []
    with tempfile.TemporaryDirectory() as tmp:
        env = kc_env.knowcode_env(tmp)
        env.pop("KNOWCODE_TOOLS", None)
        commands = cli(binary, env)
        tools, topics = mcp(binary, env, tmp)
        keys = config_keys(binary, env)
    variables = env_vars()
    contract = contract_sets()
    if not commands or not tools or not keys:
        sys.exit(f"could not read commands, tools or config keys from {binary}")
    all_flags = set().union(*commands.values()) | COMMON_FLAGS
    known_env = variables | SCRIPT_ENV
    # A flag named on its own may belong to one of the repo's scripts (`--machine-notes`).
    script_flags = set()
    for f in (ROOT / "scripts").glob("**/*.py"):
        script_flags.update(re.findall(r"add_argument\(\s*(?:'-\w',\s*|\"-\w\",\s*)?['\"](--[a-z][\w-]*)['\"]", f.read_text()))
    # Shell scripts parse their flags in a `case`: `--dry-run) …` or `-h|--help) …`.
    for f in (ROOT / "scripts").glob("**/*.sh"):
        script_flags.update(re.findall(r"(?m)^\s*(?:-\w\|)?(--[a-z][\w-]*)(?:\|[^)\s]*)?\)", f.read_text()))

    texts = {d: d.read_text() for d in docs()}
    for d, text in texts.items():
        rel = d.relative_to(ROOT)
        for n, line in enumerate(text.splitlines(), 1):
            where = f"{rel}:{n}"
            for m in re.finditer(r"\bknowcode ((?:[a-z][\w-]*)(?: [a-z][\w-]*)?)((?:[^`|\n;]*))", line):
                words = m.group(1).split()
                cmd = " ".join(words) if " ".join(words) in commands else words[0]
                if cmd not in commands:
                    if words[0] not in {"is", "and", "or", "has", "to", "on", "in", "for", "a", "the"}:
                        problems.append(f"{where}: `knowcode {words[0]}` is not a command")
                    continue
                for flag in re.findall(r"(?<![\w-])(--[a-z][\w-]*)", m.group(2)):
                    if flag not in commands[cmd] and flag not in COMMON_FLAGS:
                        problems.append(f"{where}: `knowcode {cmd}` has no flag {flag}")
            for tool in re.findall(r"\bkc_[a-z_]+(?:\.[a-z_]+)?", line):
                if tool not in tools and tool not in topics and tool not in CRATES:
                    problems.append(f"{where}: {tool} is not an MCP tool or help topic")
            for m in re.finditer(r"\b(kc_[a-z_]+)\(([^)]*)\)", line):
                for param in re.findall(r"(?:^|,\s*)([a-z_]+):", m.group(2)):
                    if m.group(1) in tools and param not in tools[m.group(1)]:
                        problems.append(f"{where}: {m.group(1)} has no parameter {param}")
            for key in re.findall(r"`((?:cache|notes|workspace|privacy|update|conventions)(?:\.[a-z_]+)+)`", line):
                if key not in keys and ".sqlite" not in key:
                    problems.append(f"{where}: config key {key} does not exist")
            for var in re.findall(r"\bKNOWCODE_[A-Z_]+\b", line):
                if var not in known_env:
                    problems.append(f"{where}: {var} is not read anywhere")
            for flag in re.findall(r"`(--[a-z][\w-]*)(?: [^`]*)?`", line):
                if flag not in all_flags and flag not in script_flags:
                    problems.append(f"{where}: no command has a flag {flag}")

    usage = (ROOT / "docs" / "usage.md").read_text()
    configuration = (ROOT / "docs" / "configuration.md").read_text()
    answers = (ROOT / "docs" / "answers.md").read_text()
    for cmd, flags in commands.items():
        parent, _, sub = cmd.partition(" ")
        listed = sub and any(f"knowcode {parent}" in l and re.search(rf"(?<![\w-]){sub}(?![\w-])", l) for l in usage.splitlines())
        if f"`{cmd}`" not in usage and f"knowcode {cmd}" not in usage and not listed:
            problems.append(f"docs/usage.md: command `{cmd}` is not documented")
        for flag in sorted(flags - COMMON_FLAGS):
            if flag not in usage:
                problems.append(f"docs/usage.md: flag {flag} of `{cmd}` is not documented")
    for tool in sorted(tools):
        if f"`{tool}`" not in usage:
            problems.append(f"docs/usage.md: MCP tool {tool} is not documented")
    for key in sorted(keys):
        if f"`{key}`" not in configuration:
            problems.append(f"docs/configuration.md: config key {key} is not documented")
    for var in sorted(variables - INTERNAL_ENV - INSTALLER_ENV):
        if var.startswith(INTERNAL_ENV_PREFIXES):
            continue
        if f"`{var}`" not in configuration:
            problems.append(f"docs/configuration.md: environment variable {var} is not documented")
    for name, members in contract.items():
        for member in sorted(members):
            if f"`{member}`" not in answers:
                problems.append(f"docs/answers.md: {name.lower()} `{member}` is not documented")
    return problems


def main():
    binary = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target" / "debug" / "knowcode")
    if not Path(binary).exists():
        sys.exit(f"{binary} not found: build it first (cargo build)")
    problems = check(str(Path(binary).resolve()))
    for p in problems:
        print(p)
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
