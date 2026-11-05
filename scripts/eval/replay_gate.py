#!/usr/bin/env python3
"""Release replay gate: KnowCode's answers on recorded calls, with no model calls.

    replay_gate.py extract --study public-v2 --transcripts DIR [DIR ...] --out corpus.jsonl
    replay_gate.py fetch --repos a,b --out SOURCE
    replay_gate.py run --knowcode BIN --label NAME --source SOURCE --work WORK [--corpus FILE ...] [--repos a,b]
    replay_gate.py compare BASE_SUMMARY CANDIDATE_SUMMARY [--recall-tolerance …]

`extract` collects every KnowCode call in a study's raw transcripts as the tool it runs and that tool's arguments
(a `kc {op: …}` call reads as its op's tool, public_transcript.knowcode_tool), one row per question and distinct
call, with session paths made repository-relative. eval/replay-gate/corpus.jsonl holds public-v1's and public-v2's.

`fetch` exports the pinned public source of some of public-v2's repositories, as prepare-public-eval.py does for all of
them: a shallow fetch of each pinned commit, exported with the manifest's exclusions. CI uses it for the fast subset
(SUBSET), cached by those commits.

`run` replays a corpus against one build. It prepares neutral Git workspaces from the pinned public source once
(APFS clones, each hashed against the source receipt), indexes them with the build into the build's own cache, starts
`BIN serve --tools all` over every workspace, and sends each call over MCP. Per question, the places (`repo/path:line`)
the answers to its calls name are scored against the study's published label (neutral/score.py). It reports, per
question type, recall and precision of those places; per tool, answer bytes; the calls that error; and the build's
tool schemas per profile: tools listed and always loaded, in bytes, and in tokens through the provider's counter when
ANTHROPIC_API_KEY is set (never otherwise; CI has no key).

Calls are replayed as the separate tool they run, which every build serves under `--tools all`; a build whose default
is the one `kc` tool runs the same code for its ops. Recorded `kc_more` calls are skipped (their cursors belong to the
recorded session), and so is `kc_note_add` (it writes).

A call KnowCode refuses by design, saying why (a `paths` filter that matches no file of the cohort), is a refusal, not
an error: it is counted in its own column.

`compare` fails (exit 1) when the candidate regresses beyond a tolerance: recall or precision per question type, calls
that error, calls the base answered and the candidate refuses, answer bytes per tool, or always-loaded schema bytes.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import select
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # scripts/: kc_env
import kc_env  # noqa: E402
import public_transcript  # noqa: E402
import recheck  # noqa: E402

STUDIES = {'public-v1': ROOT/'eval/public-v1/study', 'public-v2': ROOT/'eval/public-v2/study',
           'public-v3': ROOT/'eval/public-v3/study'}
CORPUS = ROOT/'eval/replay-gate/corpus.jsonl'
QUESTION = re.compile(r'(public\d?-(?:definition-lookup|di-implementations|http-cross-repo|two-hop-impact)-\d{3})$')
SESSION_PATH = re.compile(r'[^\s"\']*/sessions/[^/\s"\']+/(?:source|control/index)/')
SKIPPED = {'kc_more': 'its cursor belongs to the recorded session', 'kc_note_add': 'it writes a note'}
# A place an answer names: `path:line`, optionally `repo:path:line` or `repo/path:line`.
PLACE = re.compile(r'(?<![\w@./-])(?:([A-Za-z0-9][\w.-]*):)?((?:[\w@.+-]+/)*[\w@+-][\w@.+-]*\.(?:'
                   + recheck.EXTENSIONS + r')):(\d+)(?![\w/])')
# A tree row's call sites, after its caller's place: `… src/a.cs:37  [ambiguous] direct at line 46 (also 53)`.
CALL_SITES = re.compile(r'\] \w+ at line (\d+)(?: \(also ([\d, ]+)\))?')
PROFILES = ('one', 'core', 'all')
# Refusals by design: the answer says what the call asked for doesn't exist here, rather than failing.
REFUSALS = (re.compile(r'\bmatched 0 files \('),)
# Refusals by design, by code, for builds that code their errors (0.6 on); REFUSALS reads older builds' English.
REFUSAL_CODES = {'no_files'}
# The reasons an `incomplete` or `more` entry may give, and the codes it or a refused call may carry (docs/answers.md).
REASONS = {'capped', 'budget', 'deadline', 'stale', 'failed', 'missing_repo', 'unsupported', 'policy', 'withheld', 'depth'}
CODES = {'invalid_argument', 'not_found', 'ambiguous', 'no_files', 'not_indexed', 'indexing', 'gone', 'withheld',
         'stale_cursor', 'unsupported', 'collision', 'cut', 'not_allowed', 'cancelled', 'failed', 'internal'}
TREE_ENTRY = re.compile(r'^(?:incomplete|more) \((\w+)\)', re.M)
GIT_ENV = {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C', 'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_SYSTEM': '/dev/null',
           'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_TERMINAL_PROMPT': '0'}
# The fast subset: the questions whose repositories are all small. About 12 MB of source, 22 questions, 119 calls.
SUBSET = ('autofac', 'autofac-di', 'serilog', 'serilog-aspnetcore', 'serilog-extensions-hosting')
TOLERANCES = {'recall': 0.02, 'precision': 0.05, 'bytes': 0.10, 'schema': 0.05}


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def digest(data):
    return hashlib.sha256(data).hexdigest()


def jsonl(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]


# --- extract ------------------------------------------------------------------------------------------------------

def extract(study, transcript_dirs):
    """{(question, tool, args): sessions} from every transcript under the given directories."""
    if study not in STUDIES:
        raise SystemExit('unknown study ' + study)
    rows = {}
    for directory in transcript_dirs:
        for transcript in sorted(Path(directory).rglob('transcript.jsonl')):
            match = QUESTION.search(transcript.parent.name)
            if not match:
                continue
            seen = set()
            for event in recheck.read_events(transcript):
                message = event.get('message') if isinstance(event, dict) else None
                for block in message.get('content') if isinstance(message, dict) and isinstance(message.get('content'), list) else ():
                    if not (isinstance(block, dict) and block.get('type') == 'tool_use'):
                        continue
                    tool = public_transcript.knowcode_tool(block.get('name'), block.get('input'))
                    if tool is None or block.get('id') in seen:
                        continue
                    seen.add(block.get('id'))
                    args = json.loads(SESSION_PATH.sub('', json.dumps(tool[1])))
                    key = (match.group(1), tool[0], json.dumps(args, sort_keys=True))
                    rows[key] = rows.get(key, 0) + 1
    return [dict(study=study, question=q, tool=t, args=json.loads(a), sessions=n) for (q, t, a), n in sorted(rows.items())]


def fetch(repos, out, manifest=ROOT/'eval/public-v2/manifest.json'):
    """The pinned public source of `repos` (names in the manifest) in OUT: objects/, view/ and source-receipt.json."""
    spec = json.loads(Path(manifest).read_text())
    families = [dict(family, repos=[r for r in family['repos'] if r['name'] in repos]) for family in spec['families']]
    families = [f for f in families if f['repos']]
    found = {r['name'] for f in families for r in f['repos']}
    if found != set(repos):
        raise SystemExit('not in the manifest: ' + ', '.join(sorted(set(repos) - found)))
    prepare = load('replay_gate_prepare', ROOT/'scripts/prepare-public-eval.py')
    with tempfile.TemporaryDirectory(prefix='replay-gate-manifest-') as temp:
        subset = Path(temp)/'manifest.json'
        subset.write_text(json.dumps(dict(spec, families=families), indent=2) + '\n')
        prepare.prepare(subset, Path(out))


# --- workspaces and index ------------------------------------------------------------------------------------------

def workspaces(source, work, only=None):
    """Neutral Git workspaces of the pinned source (or of the repositories in `only`) under WORK, made once per source
    receipt and repository set."""
    census = load('replay_gate_census', ROOT/'scripts/census-public-eval.py')
    receipt_bytes = (Path(source)/'source-receipt.json').read_bytes()
    receipt = json.loads(receipt_bytes)
    repos = [r for r in receipt['repos'] if only is None or r['name'] in only]
    if only is not None and {r['name'] for r in repos} != set(only):
        raise SystemExit('unknown repositories: ' + ', '.join(sorted(set(only) - {r['name'] for r in repos})))
    out = Path(work)/('workspaces' if only is None else 'workspaces-' + digest(','.join(sorted(only)).encode())[:8])
    marker = out/'.source-receipt-sha256'
    if marker.is_file() and marker.read_text().strip() == digest(receipt_bytes):
        return {r['name']: out/r['name'] for r in repos}
    if out.exists():
        for directory, _, _ in os.walk(out):
            Path(directory).chmod(0o755)
        shutil.rmtree(out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.replay-gate-', dir=out.parent) as temp:
        stage = Path(temp)/'workspaces'
        template = Path(temp)/'template'
        template.mkdir()
        for repo in repos:
            target = stage/repo['name']
            target.mkdir(parents=True)
            for item in repo['files']:
                if item['symlink']:
                    raise SystemExit('replay workspaces need a regular-file cohort')
                name = census.safe_relative(item['path']).as_posix()
                (target/name).parent.mkdir(parents=True, exist_ok=True)
                census.clone_or_copy(Path(source)/'view'/repo['name']/name, target/name)
                (target/name).chmod(0o444)
            census.inspect_view(target, repo, reread=False)
            for args in (['init', '--quiet', '--initial-branch=public-eval', '--template=' + str(template)],
                         ['config', '--local', 'core.hooksPath', '/dev/null']):
                subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', *args], cwd=target, env=GIT_ENV, check=True,
                               capture_output=True)
        (stage/'.source-receipt-sha256').write_text(digest(receipt_bytes) + '\n')
        stage.rename(out)
    return {r['name']: out/r['name'] for r in repos}


def build_env(base):
    # On the bare git environment, not the caller's: a run depends on nothing of the machine's user.
    return kc_env.knowcode_env(base, start=GIT_ENV)


# --- MCP ------------------------------------------------------------------------------------------------------------

class Server:
    """`BIN serve` over stdio, spoken to as an MCP client: one request at a time."""

    def __init__(self, binary, args, env, cwd):
        self.process = subprocess.Popen([str(binary), 'serve', *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.DEVNULL, env=env, cwd=cwd)
        self.next_id = 0
        self.buffer = b''
        init = self.request('initialize', {'protocolVersion': '2025-03-26', 'capabilities': {},
                                           'clientInfo': {'name': 'replay_gate', 'version': '1'}}, timeout=120)
        self.instructions = (init.get('result') or {}).get('instructions') or ''
        self.send({'jsonrpc': '2.0', 'method': 'notifications/initialized'})
        self.tools = (self.request('tools/list', {}, timeout=60).get('result') or {}).get('tools') or []

    def send(self, message):
        self.process.stdin.write((json.dumps(message) + '\n').encode())
        self.process.stdin.flush()

    def request(self, method, params, timeout):
        self.next_id += 1
        self.send({'jsonrpc': '2.0', 'id': self.next_id, 'method': method, 'params': params})
        until = time.monotonic() + timeout
        while True:
            while b'\n' in self.buffer:
                line, self.buffer = self.buffer.split(b'\n', 1)
                if line.strip():
                    message = json.loads(line)
                    if message.get('id') == self.next_id:
                        return message
            remaining = until - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(method)
            ready, _, _ = select.select([self.process.stdout], [], [], remaining)
            if ready:
                chunk = os.read(self.process.stdout.fileno(), 1 << 20)
                if not chunk:
                    raise RuntimeError('server exited')
                self.buffer += chunk

    def call(self, tool, args, timeout=120):
        error, text, _ = self.call_coded(tool, args, timeout)
        return error, text

    def call_coded(self, tool, args, timeout=120):
        """(whether it was an error, its text, the error's code when the build gives one)."""
        reply = self.request('tools/call', {'name': tool, 'arguments': args}, timeout)
        result = reply.get('result') or {}
        text = '\n'.join(part.get('text', '') for part in result.get('content') or [] if part.get('type') == 'text')
        if 'error' in reply:
            return True, json.dumps(reply['error']), None
        code = ((result.get('structuredContent') or {}).get('error') or {}).get('code')
        return bool(result.get('isError')), text, code

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def schemas(binary, key):
    """Per profile the build serves: tools listed and always loaded, their schema bytes, and the instructions."""
    out = {}
    with tempfile.TemporaryDirectory(prefix='replay-gate-schema-') as temp:
        env = build_env(Path(temp))
        for profile in (None,) + PROFILES:
            try:
                server = Server(binary, [] if profile is None else ['--tools', profile], env, temp)
            except (RuntimeError, TimeoutError, json.JSONDecodeError):
                continue
            tools, instructions = server.tools, server.instructions
            server.close()
            if not tools:
                continue
            loaded = [t for t in tools if (t.get('_meta') or {}).get('anthropic/alwaysLoad') is True]
            row = dict(tools=len(tools), tool_bytes=len(json.dumps(tools)), always_loaded=len(loaded),
                       always_loaded_bytes=len(json.dumps(loaded)), instructions_bytes=len(instructions.encode()))
            if key:
                cost = load('replay_gate_schema_cost', HERE/'schema_cost.py')
                base_tokens = cost.count(cost.request('claude-opus-5-5'), key)
                row['always_loaded_tokens'] = cost.count(cost.request('claude-opus-5-5', [cost.api_tool(t) for t in loaded],
                                                                      instructions), key) - base_tokens
            out['default' if profile is None else profile] = row
    return out


# --- run ------------------------------------------------------------------------------------------------------------

def json_places(value, roots):
    """The places a JSON answer gives as fields: every object with a `file` and a `line`, in the object's own `repo`
    when that names a repository of the cohort (by name, or as its root's last component). An object with a file and
    no line names no place."""
    out = []
    if isinstance(value, dict):
        file, line, repo = value.get('file'), value.get('line'), value.get('repo')
        if isinstance(file, str) and file and isinstance(line, int) and not isinstance(line, bool) and line > 0:
            name = repo if isinstance(repo, str) and repo in roots else None
            if name is None and isinstance(repo, str) and Path(repo).name in roots:
                name = Path(repo).name
            out.append((name, file, str(line)))
        for v in value.values():
            out += json_places(v, roots)
    elif isinstance(value, list):
        for v in value:
            out += json_places(v, roots)
    return out


def places(text, repos, roots):
    """The `repo/path:line` places an answer names, whatever its format: in its text (a `path:line`, and a tree row's
    call sites, `at line N (also M)`, in its caller's file) and, for a JSON answer, in its rows' `file` and `line`
    fields. A path without its repository is given the one repository, among the question's first and then the
    cohort's, that has the file."""
    named = []
    for row in text.splitlines():
        matches = list(PLACE.finditer(row))
        named += [m.groups() for m in matches]
        sites = CALL_SITES.search(row)
        if matches and sites:
            repo, path = matches[-1].group(1), matches[-1].group(2)
            named += [(repo, path, line) for line in [sites.group(1), *re.findall(r'\d+', sites.group(2) or '')]]
    try:
        named += json_places(json.loads(text), roots)
    except ValueError:
        pass
    out = []
    for repo, path, line in named:
        first = path.split('/', 1)[0]
        if repo in roots:
            out.append(f'{repo}/{path}:{line}')
        elif first in roots and '/' in path:
            out.append(f'{path}:{line}')
        else:
            owners = [r for r in repos if (roots[r]/path).is_file()] or [r for r in roots if (roots[r]/path).is_file()]
            out.append(f'{owners[0]}/{path}:{line}' if len(owners) == 1 else f'{path}:{line}')
    return out


def run(args):
    started = time.monotonic()
    work = Path(args.work).resolve()
    binary = Path(args.knowcode).resolve()
    corpus = [row for path in (args.corpus or [CORPUS]) for row in jsonl(path)]
    banks = {}
    for study in sorted({row['study'] for row in corpus}):
        banks[study] = ({q['id']: q for q in jsonl(STUDIES[study]/'questions.jsonl')},
                        {l['id']: l for l in jsonl(STUDIES[study]/'labels.jsonl')})
    keep = set(args.repos.split(',')) if args.repos else None
    if keep:
        corpus = [row for row in corpus if set(banks[row['study']][0][row['question']]['repos']) <= keep]
    roots = workspaces(args.source, work, keep)
    timing = {'workspaces_s': round(time.monotonic() - started, 1)}
    # Named by the binary's hash, not the label: answers that report the cache path (kc_status) then have the same
    # length for every build, so a replay is byte-for-byte repeatable.
    base = work/'builds'/digest(binary.read_bytes())[:12]
    if base.exists():
        shutil.rmtree(base)
    env = build_env(base)
    t0 = time.monotonic()
    indexed = subprocess.run([str(binary), 'index', *map(str, roots.values())], env=env, cwd=base, capture_output=True, text=True)
    if indexed.returncode != 0:
        raise SystemExit('index failed: ' + indexed.stderr[-500:])
    timing['index_s'] = round(time.monotonic() - t0, 1)
    version = subprocess.run([str(binary), '--version'], env=env, capture_output=True, text=True).stdout.strip()
    t0 = time.monotonic()
    server = Server(binary, ['--tools', 'all', *[a for r in roots.values() for a in ('--repo', str(r))]], env, base)
    calls, skipped = [], {}
    try:
        for row in corpus:
            if row['tool'] in SKIPPED:
                skipped[row['tool']] = skipped.get(row['tool'], 0) + row['sessions']
                continue
            question = banks[row['study']][0][row['question']]
            try:
                error, text, code = server.call_coded(row['tool'], row['args'])
                refused = error and (code in REFUSAL_CODES or any(r.search(text) for r in REFUSALS))
                error = error and not refused
            except TimeoutError:
                error, refused, text, code = True, False, 'timeout', None
            calls.append(dict(study=row['study'], question=row['question'], stratum=question['stratum'], tool=row['tool'],
                              args=row['args'], error=error, refused=refused, code=code, entries=entry_codes(text),
                              bytes=len(text.encode()), places=[] if error else places(text, question['repos'], roots)))
    finally:
        server.close()
    timing['calls_s'] = round(time.monotonic() - t0, 1)
    t0 = time.monotonic()
    profile_schemas = schemas(binary, os.environ.get('ANTHROPIC_API_KEY'))
    timing['schemas_s'] = round(time.monotonic() - t0, 1)
    scorer = load('replay_gate_score', ROOT/'scripts/eval/neutral/score.py')
    questions = {}
    for call in calls:
        questions.setdefault((call['study'], call['question']), []).append(call)
    per_question = []
    for (study, qid), qcalls in sorted(questions.items()):
        question, label = banks[study][0][qid], banks[study][1][qid]
        named = sorted({p for c in qcalls for p in c['places']})
        score = scorer.score_one(label, named, question['repos'])
        per_question.append(dict(study=study, question=qid, stratum=question['stratum'], calls=len(qcalls),
                                 places=len(named), recall=score['recall'], precision=score['precision'],
                                 missed=score['missed']))
    strata = {}
    for row in per_question:
        s = strata.setdefault(row['stratum'], dict(questions=0, recall=[], precision=[]))
        s['questions'] += 1
        s['recall'].append(row['recall'])
        if row['precision'] is not None:
            s['precision'].append(row['precision'])
    strata = {k: dict(questions=v['questions'], recall=round(statistics.mean(v['recall']), 4),
                      precision=round(statistics.mean(v['precision']), 4) if v['precision'] else None,
                      precision_questions=len(v['precision'])) for k, v in sorted(strata.items())}
    tools = {}
    for call in calls:
        t = tools.setdefault(call['tool'], dict(calls=0, errors=0, bytes=[]))
        t['calls'] += 1
        t['errors'] += call['error']
        t['refusals'] = t.get('refusals', 0) + call['refused']
        t['bytes'].append(call['bytes'])
    tools = {k: dict(calls=v['calls'], errors=v['errors'], refusals=v['refusals'], bytes_total=sum(v['bytes']),
                     bytes_median=statistics.median(v['bytes'])) for k, v in sorted(tools.items())}
    timing['total_s'] = round(time.monotonic() - started, 1)
    summary = dict(format='replay-gate-1', label=args.label, knowcode_version=version,
                   knowcode_sha256=digest(binary.read_bytes()), corpus_rows=len(corpus), calls=len(calls),
                   skipped=skipped, repos=sorted(roots), strata=strata, tools=tools,
                   errors=[dict(question=c['question'], tool=c['tool'], args=c['args']) for c in calls if c['error']],
                   refusals=[dict(question=c['question'], tool=c['tool'], args=c['args']) for c in calls if c['refused']],
                   coded=any(c['code'] for c in calls), uncoded=uncoded(calls),
                   schemas=profile_schemas, timing=timing, questions=per_question)
    out = Path(args.out) if args.out else base/'summary.json'
    out.write_text(json.dumps(summary, indent=2, sort_keys=True) + '\n')
    (base/'calls.jsonl').write_text(''.join(json.dumps(c, sort_keys=True) + '\n' for c in calls))
    return summary


def entry_codes(text):
    """The codes of an answer's `incomplete` and `more` entries: each one's `code`, else its `reason`; from a tree's
    header lines when the answer is text."""
    try:
        v = json.loads(text)
    except ValueError:
        return sorted(set(TREE_ENTRY.findall(text)))
    found = set()

    def walk(x):
        if isinstance(x, dict):
            for key in ('incomplete', 'more'):
                for e in x.get(key) or []:
                    if isinstance(e, dict):
                        found.add(e.get('code') or e.get('reason') or '')
            for y in x.values():
                walk(y)
        elif isinstance(x, list):
            for y in x:
                walk(y)
    walk(v)
    return sorted(found)


def uncoded(calls):
    """The calls whose refusal has no code (or `internal`), or that give an entry a reason or code outside the closed
    sets: every one of them is something a client can't act on without reading the English."""
    out = []
    for c in calls:
        bad = [e for e in c.get('entries', []) if e not in REASONS and e not in CODES]
        if (c['error'] or c['refused']) and c.get('code') in (None, 'internal'):
            bad.append(f"refused with code {c.get('code')}")
        if bad:
            out.append(dict(question=c['question'], tool=c['tool'], args=c['args'], why=bad))
    return out


# --- compare --------------------------------------------------------------------------------------------------------

def compare(base, cand, tolerances=TOLERANCES):
    """Regressions of cand against base, beyond the tolerances, as readable lines."""
    out = []
    for stratum, b in base['strata'].items():
        c = cand['strata'].get(stratum)
        if c is None:
            out.append(f'{stratum}: missing from the candidate')
            continue
        if c['recall'] < b['recall'] - tolerances['recall']:
            out.append(f"{stratum}: recall {b['recall']:.3f} -> {c['recall']:.3f}")
        if b['precision'] is not None and (c['precision'] or 0) < b['precision'] - tolerances['precision']:
            out.append(f"{stratum}: precision {b['precision']:.3f} -> {c['precision']}")
    key = lambda e: (e['question'], e['tool'], json.dumps(e['args'], sort_keys=True))
    failed = {key(e) for e in base['errors']}
    for e in cand['errors']:
        if key(e) not in failed:
            out.append(f"new error: {e['tool']} {json.dumps(e['args'], sort_keys=True)} ({e['question']})")
    # A call the base refused by design may stay refused; one it answered must not become a refusal.
    refused = {key(e) for e in base.get('refusals', [])} | failed
    for e in cand.get('refusals', []):
        if key(e) not in refused:
            out.append(f"newly refused: {e['tool']} {json.dumps(e['args'], sort_keys=True)} ({e['question']})")
    # A build that codes its refusals codes every one, and every entry's reason is one the contract names.
    if cand.get('coded'):
        for e in cand.get('uncoded', []):
            out.append(f"uncoded: {e['tool']} {json.dumps(e['args'], sort_keys=True)} ({e['question']}): {', '.join(e['why'])}")
    for tool, b in base['tools'].items():
        c = cand['tools'].get(tool)
        if c and b['bytes_total'] and c['bytes_total'] > b['bytes_total'] * (1 + tolerances['bytes']):
            out.append(f"{tool}: answer bytes {b['bytes_total']} -> {c['bytes_total']}")
    b, c = base['schemas'].get('default'), cand['schemas'].get('default')
    if b and c:
        before, after = b['always_loaded_bytes'] + b['instructions_bytes'], c['always_loaded_bytes'] + c['instructions_bytes']
        if after > before * (1 + tolerances['schema']):
            out.append(f'always-loaded schemas and instructions: {before} -> {after} bytes')
    return out


def table(base, cand):
    lines = [f"{base['label']} ({base['knowcode_version']}) vs {cand['label']} ({cand['knowcode_version']})", '',
             '| Question type | Questions | Recall | Precision |', '| --- | ---: | ---: | ---: |']
    for stratum, b in base['strata'].items():
        c = cand['strata'].get(stratum, {})
        fmt = lambda v: '—' if v is None else f'{v:.3f}'
        lines.append(f"| {stratum} | {b['questions']} | {fmt(b['recall'])} → {fmt(c.get('recall'))} | "
                     f"{fmt(b['precision'])} → {fmt(c.get('precision'))} |")
    lines += ['', '| Tool | Calls | Errors | Refusals | Answer bytes (total) | Median |', '| --- | ---: | ---: | ---: | ---: | ---: |']
    for tool, b in base['tools'].items():
        c = cand['tools'].get(tool, {})
        lines.append(f"| {tool} | {b['calls']} | {b['errors']} → {c.get('errors')} | {b.get('refusals', 0)} → {c.get('refusals', 0)} | {b['bytes_total']} → "
                     f"{c.get('bytes_total')} | {b['bytes_median']} → {c.get('bytes_median')} |")
    lines += ['', '| Profile | Tools listed | Always loaded | Always-loaded bytes | Instructions bytes | Tokens |',
              '| --- | ---: | ---: | ---: | ---: | ---: |']
    for name, build in (('base', base), ('candidate', cand)):
        for profile, s in build['schemas'].items():
            lines.append(f"| {name} {profile} | {s['tools']} | {s['always_loaded']} | {s['always_loaded_bytes']} | "
                         f"{s['instructions_bytes']} | {s.get('always_loaded_tokens', '—')} |")
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    sub = parser.add_subparsers(dest='command', required=True)
    e = sub.add_parser('extract')
    e.add_argument('--study', required=True, choices=sorted(STUDIES))
    e.add_argument('--transcripts', nargs='+', required=True)
    e.add_argument('--out', required=True)
    f = sub.add_parser('fetch')
    f.add_argument('--repos', default=','.join(SUBSET), help='comma-separated public-v2 repositories (default: the fast subset)')
    f.add_argument('--out', required=True, help='a new or empty directory outside this checkout')
    r = sub.add_parser('run')
    r.add_argument('--knowcode', required=True)
    r.add_argument('--label', required=True)
    r.add_argument('--source', required=True, help='the pinned public source (source-receipt.json and view/)')
    r.add_argument('--work', required=True, help='workspaces and per-build caches; never a user repository')
    r.add_argument('--corpus', nargs='*', help=f'call corpora (default {CORPUS.relative_to(ROOT)})')
    r.add_argument('--repos', help='only questions whose repositories are all in this comma-separated list')
    r.add_argument('--out')
    c = sub.add_parser('compare')
    c.add_argument('base')
    c.add_argument('candidate')
    for name, value in TOLERANCES.items():
        c.add_argument(f'--{name}-tolerance', type=float, default=value)
    c.add_argument('--out', help='write the comparison table here')
    a = parser.parse_args()
    if a.command == 'extract':
        rows = extract(a.study, a.transcripts)
        Path(a.out).write_text(''.join(json.dumps(r, sort_keys=True) + '\n' for r in rows))
        print(json.dumps(dict(rows=len(rows), calls=sum(r['sessions'] for r in rows))))
    elif a.command == 'fetch':
        fetch(a.repos.split(','), a.out)
    elif a.command == 'run':
        summary = run(a)
        print(json.dumps({k: summary[k] for k in ('label', 'knowcode_version', 'calls', 'skipped', 'strata', 'timing')}, indent=1))
    else:
        base, cand = json.loads(Path(a.base).read_text()), json.loads(Path(a.candidate).read_text())
        text = table(base, cand)
        regressions = compare(base, cand, {k: getattr(a, f'{k}_tolerance') for k in TOLERANCES})
        text += '\n' + ('\n'.join('REGRESSION: ' + r for r in regressions) if regressions else 'no regression beyond tolerance') + '\n'
        print(text, end='')
        if a.out:
            Path(a.out).write_text(text)
        sys.exit(1 if regressions else 0)


if __name__ == '__main__':
    main()
