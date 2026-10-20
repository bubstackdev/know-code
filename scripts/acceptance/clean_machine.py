#!/usr/bin/env python3
"""Parsing helpers for clean-machine.sh: the machine record, the acceptance manifest, Claude Code's registration
listings, per-entry settings comparison, session transcripts and the final summary.

Standard library only (Python 3.9+). Each subcommand reads files and writes JSON; none changes client state.
"""
from __future__ import annotations

import argparse
import datetime as dt
import glob
import json
import os
import platform
import re
import subprocess
import sys
from pathlib import Path

PLUGIN = "knowcode@knowcode-local"
MARKETPLACE = "knowcode-local"
# The only settings entries `knowcode setup claude` adds: everything else must survive uninstall unchanged.
KNOWCODE_ENTRIES = (("enabledPlugins", PLUGIN), ("extraKnownMarketplaces", MARKETPLACE))
NAMED_PROMPT = "Use the KnowCode kc_context tool to answer: {question}"


# ---------------------------------------------------------------------------------------------------------- machine

def _run(argv: list[str]) -> str:
    try:
        return subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def machine_record(quiet: str, notes: str, client_version: str) -> dict:
    system = platform.system()
    record = {"os": system, "os_release": platform.release(), "arch": platform.machine(),
              "cpu_count": os.cpu_count(), "load_average": list(os.getloadavg()),
              "client_version": client_version, "quiet_confirmed_by_operator": quiet == "yes",
              "operator_notes": notes, "python": platform.python_version(),
              "recorded_utc": dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")}
    if system == "Darwin":
        record["os_version"] = _run(["sw_vers", "-productVersion"])
        record["cpu"] = _run(["sysctl", "-n", "machdep.cpu.brand_string"])
        memory = _run(["sysctl", "-n", "hw.memsize"])
        record["memory_bytes"] = int(memory) if memory.isdigit() else None
        # 1 when this process runs under Rosetta translation.
        record["translated"] = _run(["sysctl", "-n", "sysctl.proc_translated"]) == "1"
    else:
        try:
            fields = dict(line.split("=", 1) for line in Path("/etc/os-release").read_text().splitlines() if "=" in line)
            record["os_version"] = fields.get("PRETTY_NAME", "").strip('"')
        except OSError:
            record["os_version"] = ""
        record["cpu"] = ""
        try:
            for line in Path("/proc/cpuinfo").read_text().splitlines():
                if line.lower().startswith(("model name", "cpu model")):
                    record["cpu"] = line.split(":", 1)[1].strip()
                    break
        except OSError:
            pass
        record["memory_bytes"] = None
        try:
            for line in Path("/proc/meminfo").read_text().splitlines():
                if line.startswith("MemTotal:"):
                    record["memory_bytes"] = int(line.split()[1]) * 1024
        except (OSError, ValueError):
            pass
        record["container"] = Path("/.dockerenv").exists() or Path("/run/.containerenv").exists()
    return record


def environment_label(system: str, arch: str) -> str:
    """The check-public-first-run.py --environment label for this platform."""
    if system == "Darwin" and arch == "arm64":
        return "fresh-macos-account"
    if system == "Linux" and arch == "x86_64":
        return "clean-linux-x86_64-container"
    if system == "Linux" and arch in ("aarch64", "arm64"):
        return "clean-linux-arm64"
    return "isolated-profile"


# --------------------------------------------------------------------------------------------- client registration

def _load_json(path: str | Path):
    try:
        text = Path(path).read_text()
    except FileNotFoundError:
        return None
    return json.loads(text) if text.strip() else None


def registrations(plugins, marketplaces) -> list[str]:
    """KnowCode's entries in `claude plugin list --json` and `claude plugin marketplace list --json`."""
    found = [f"plugin {p.get('id')}" for p in plugins or [] if isinstance(p, dict) and
             str(p.get("id", "")).endswith("@" + MARKETPLACE)]
    found += [f"marketplace {m.get('name')}" for m in marketplaces or [] if isinstance(m, dict) and
              m.get("name") == MARKETPLACE]
    return found


def settings_entries(settings) -> list[str]:
    """KnowCode's own entries still present in a Claude settings object."""
    settings = settings if isinstance(settings, dict) else {}
    return [f"{key}[{name!r}]" for key, name in KNOWCODE_ENTRIES
            if isinstance(settings.get(key), dict) and name in settings[key]]


# ------------------------------------------------------------------------------------------------- settings compare

def _without_knowcode(settings: dict) -> dict:
    out = json.loads(json.dumps(settings))
    for key, name in KNOWCODE_ENTRIES:
        if isinstance(out.get(key), dict):
            out[key].pop(name, None)
    return out


def compare_settings(before, after) -> dict:
    """Per-entry comparison of client settings snapshotted right before uninstall with those after it.

    Uninstall may remove only KnowCode's own entries (`KNOWCODE_ENTRIES`). Every other entry must be unchanged,
    compared per entry inside object-valued keys, so a key that still holds other entries is never reported as
    changed as a whole, and a dropped key is reported as each entry it held. A key whose object uninstall left empty
    may be dropped by the client: an empty object and a missing key are the same.
    """
    before = before if isinstance(before, dict) else {}
    after = after if isinstance(after, dict) else {}
    expected = _without_knowcode(before)
    removed = [f"{key}[{name!r}]" for key, name in KNOWCODE_ENTRIES
               if isinstance(before.get(key), dict) and name in before[key]]
    differences = []
    for key in sorted(set(expected) | set(after)):
        old, new = expected.get(key), after.get(key)
        # A missing key and an object are compared per entry, as if the key held an empty object.
        if isinstance(old, dict) and (isinstance(new, dict) or key not in after) \
                or key not in expected and isinstance(new, dict):
            old, new = old or {}, new or {}
            for entry in sorted(set(old) | set(new)):
                if entry not in new:
                    differences.append({"entry": f"{key}[{entry!r}]", "change": "removed", "before": old[entry]})
                elif entry not in old:
                    if (key, entry) in KNOWCODE_ENTRIES:
                        continue  # reported below as still present
                    differences.append({"entry": f"{key}[{entry!r}]", "change": "added", "after": new[entry]})
                elif old[entry] != new[entry]:
                    differences.append({"entry": f"{key}[{entry!r}]", "change": "changed",
                                        "before": old[entry], "after": new[entry]})
        elif key not in after:
            differences.append({"entry": key, "change": "removed", "before": old})
        elif key not in expected:
            differences.append({"entry": key, "change": "added", "after": new})
        elif old != new:
            differences.append({"entry": key, "change": "changed", "before": old, "after": new})
    remaining = settings_entries(after)
    return {"knowcode_entries_removed": removed, "knowcode_entries_remaining": remaining,
            "unrelated_differences": differences, "preserved": not differences and not remaining}


# ---------------------------------------------------------------------------------------------------- transcripts

def _time(stamp) -> float | None:
    if not isinstance(stamp, str):
        return None
    try:
        return dt.datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def _prompt_text(message) -> str | None:
    """The operator's typed prompt in a user message, or None for tool results and other non-prompt content."""
    content = message.get("content") if isinstance(message, dict) else None
    if isinstance(content, str):
        text = content
    elif isinstance(content, list):
        if any(isinstance(block, dict) and block.get("type") == "tool_result" for block in content):
            return None
        text = "\n".join(block.get("text", "") for block in content if isinstance(block, dict) and block.get("type") == "text")
    else:
        return None
    text = text.strip()
    # Slash-command echoes, local command output and injected reminders are not prompts.
    if not text or text.startswith(("<command-", "<local-command", "<system-reminder>", "Caveat:")):
        return None
    return text


def _normalize(text: str) -> str:
    return re.sub(r"\s+", " ", text or "").strip().strip("\"'")


def is_knowcode_tool(name: str, tool_input) -> str | None:
    """'mcp' for a KnowCode MCP tool, 'cli' for a Bash call that runs knowcode, otherwise None."""
    if re.match(r"^mcp__.*knowcode.*__kc_\w+$", name or ""):
        return "mcp"
    if name == "Bash" and isinstance(tool_input, dict) and re.search(r"(^|[\s;&|(/])knowcode(\s|$)", str(tool_input.get("command", ""))):
        return "cli"
    return None


def parse_transcript(path: str | Path) -> dict:
    """One session from a Claude Code transcript (JSON lines): its first prompt, tool calls and timing."""
    lines = []
    for raw in Path(path).read_text(errors="replace").splitlines():
        try:
            entry = json.loads(raw)
        except ValueError:
            continue
        if isinstance(entry, dict):
            lines.append(entry)
    session = {"transcript": str(path), "session_id": None, "cwd": None, "prompt": None, "used_knowcode": False,
               "kc_calls": [], "other_tool_calls": {}, "subagent_tool_calls": 0, "final_answer": None,
               "seconds_to_final_answer": None}
    started = None
    for entry in lines:
        session["session_id"] = session["session_id"] or entry.get("sessionId")
        session["cwd"] = session["cwd"] or entry.get("cwd")
        kind, message = entry.get("type"), entry.get("message")
        if kind == "user" and session["prompt"] is None and not entry.get("isMeta") and not entry.get("isSidechain"):
            text = _prompt_text(message)
            if text is not None:
                session["prompt"], started = text, _time(entry.get("timestamp"))
        if kind != "assistant" or not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        for block in message["content"]:
            if not isinstance(block, dict):
                continue
            if block.get("type") == "tool_use":
                if entry.get("isSidechain"):
                    session["subagent_tool_calls"] += 1
                which = is_knowcode_tool(block.get("name", ""), block.get("input"))
                if which:
                    tool = block["name"] if which == "mcp" else "Bash: " + str(block["input"].get("command", ""))[:200]
                    session["kc_calls"].append(tool)
                else:
                    name = block.get("name", "?")
                    session["other_tool_calls"][name] = session["other_tool_calls"].get(name, 0) + 1
            elif block.get("type") == "text" and block.get("text", "").strip() and not entry.get("isSidechain"):
                session["final_answer"] = block["text"].strip()
                ended = _time(entry.get("timestamp"))
                if started is not None and ended is not None:
                    session["seconds_to_final_answer"] = round(ended - started, 1)
    # Subagents' transcripts sit beside the session's in <session id>/subagents/ in newer client versions.
    if session["session_id"]:
        for sub in sorted(Path(path).parent.glob(f"{session['session_id']}/subagents/*.jsonl")):
            nested = parse_transcript(sub)
            session["subagent_tool_calls"] += len(nested["kc_calls"]) + sum(nested["other_tool_calls"].values())
            session["kc_calls"] += nested["kc_calls"]
    session["used_knowcode"] = bool(session["kc_calls"])
    session["kc_call_count"] = len(session["kc_calls"])
    session["other_tool_call_count"] = sum(session["other_tool_calls"].values())
    return session


def find_sessions(projects: str | Path, case_dir: str | Path, prompts: dict, since: float = 0) -> dict:
    """For each label in `prompts`, the latest session run in `case_dir` (after `since`) that began with that prompt."""
    case_dir = os.path.realpath(case_dir)
    found: dict = {label: None for label in prompts}
    candidates = []
    for path in glob.glob(os.path.join(str(projects), "*", "*.jsonl")):
        if os.path.getmtime(path) < since:
            continue
        session = parse_transcript(path)
        if not session["cwd"] or os.path.realpath(session["cwd"]) != case_dir or session["prompt"] is None:
            continue
        candidates.append((os.path.getmtime(path), session))
    for _, session in sorted(candidates, key=lambda c: c[0]):
        for label, prompt in prompts.items():
            if _normalize(session["prompt"]) == _normalize(prompt):
                found[label] = session
    return found


# -------------------------------------------------------------------------------------------------------- commands

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("machine")
    p.add_argument("--quiet", choices=["yes", "no"], required=True)
    p.add_argument("--notes", default="")
    p.add_argument("--client-version", default="")
    p = sub.add_parser("environment")
    p = sub.add_parser("manifest")
    p.add_argument("manifest")
    p.add_argument("case", nargs="?")
    p.add_argument("field", nargs="?")
    p = sub.add_parser("registrations")
    p.add_argument("plugins")
    p.add_argument("marketplaces")
    p.add_argument("settings")
    p = sub.add_parser("compare-settings")
    p.add_argument("before")
    p.add_argument("after")
    p = sub.add_parser("sessions")
    p.add_argument("--projects", required=True)
    p.add_argument("--case-dir", required=True)
    p.add_argument("--question", required=True)
    p.add_argument("--since", type=float, default=0)
    p = sub.add_parser("session-line")
    p.add_argument("sessions")
    p = sub.add_parser("summary")
    p.add_argument("report_dir")
    args = parser.parse_args(argv)

    if args.command == "machine":
        print(json.dumps(machine_record(args.quiet, args.notes, args.client_version), indent=2))
    elif args.command == "environment":
        print(environment_label(platform.system(), platform.machine()))
    elif args.command == "manifest":
        cases = json.loads(Path(args.manifest).read_text())
        if args.case is None:
            print("\n".join(cases))
        else:
            value = cases[args.case].get(args.field, "")
            print(value if isinstance(value, str) else json.dumps(value))
    elif args.command == "registrations":
        found = registrations(_load_json(args.plugins), _load_json(args.marketplaces))
        found += [f"settings {entry}" for entry in settings_entries(_load_json(args.settings))]
        print("\n".join(found))
        return 1 if found else 0
    elif args.command == "compare-settings":
        result = compare_settings(_load_json(args.before), _load_json(args.after))
        print(json.dumps(result, indent=2))
        return 0 if result["preserved"] else 1
    elif args.command == "sessions":
        prompts = {"unprompted": args.question, "named": NAMED_PROMPT.format(question=args.question)}
        found = find_sessions(args.projects, args.case_dir, prompts, args.since)
        report = {}
        for label, session in found.items():
            if session is None:
                report[label] = {"prompt": prompts[label], "found": False}
            else:
                report[label] = {"found": True, **{k: session[k] for k in (
                    "prompt", "used_knowcode", "kc_call_count", "kc_calls", "other_tool_call_count", "other_tool_calls",
                    "subagent_tool_calls", "seconds_to_final_answer", "final_answer", "session_id", "transcript")}}
        print(json.dumps(report, indent=2))
    elif args.command == "session-line":
        print("; ".join(f"{label}: found={s.get('found')} used_knowcode={s.get('used_knowcode')} "
                        f"kc={s.get('kc_call_count')} other={s.get('other_tool_call_count')} "
                        f"seconds={s.get('seconds_to_final_answer')}"
                        for label, s in (_load_json(args.sessions) or {}).items()))
    elif args.command == "summary":
        print(summary(Path(args.report_dir)))
    return 0


def summary(report: Path) -> str:
    """A readable summary of steps.tsv and the per-case session reports."""
    out = ["# Clean-machine acceptance", ""]
    machine = _load_json(report / "machine.json") or {}
    out.append(f"- Machine: {machine.get('os')} {machine.get('os_version', '')} {machine.get('arch')}, "
               f"{machine.get('cpu', '')}, load {machine.get('load_average')}, "
               f"quiet: {machine.get('quiet_confirmed_by_operator')}, client {machine.get('client_version')}")
    out += ["", "| Step | Result | Detail |", "| --- | --- | --- |"]
    failed = 0
    steps = report / "steps.tsv"
    for line in steps.read_text().splitlines() if steps.exists() else []:
        step, result, detail = (line.split("\t") + ["", ""])[:3]
        failed += result == "FAIL"
        out.append(f"| {step} | {result} | {detail} |")
    out += ["", "## Sessions (recorded, not gated)", "",
            "| Case | Session | Found | used_knowcode | kc calls | Other tool calls | Seconds to final answer |",
            "| --- | --- | --- | --- | --- | --- | --- |"]
    for path in sorted(report.glob("sessions-*.json")):
        case = path.stem[len("sessions-"):]
        for label, s in (_load_json(path) or {}).items():
            out.append(f"| {case} | {label} | {s.get('found')} | {s.get('used_knowcode', '')} | "
                       f"{s.get('kc_call_count', '')} | {s.get('other_tool_call_count', '')} | "
                       f"{s.get('seconds_to_final_answer', '')} |")
    out += ["", f"Gated steps failed: {failed}"]
    return "\n".join(out)


if __name__ == "__main__":
    sys.exit(main())
