#!/usr/bin/env python3
"""contract-v1: server commits that remove or change an HTTP route, and the requests in another public repository
they break, labelled by reading both sides' source at pinned commits (eval/contract-v1/README.md).

    contract_labels.py survey --work WORK [--pair jellyfin] [--out SURVEY.jsonl]

The pairs (PAIRS) are a server repository and a client repository cloned with their full history into WORK/clones
(`task_gate.py fetch` for jellyfin's). For each non-merge server commit since SINCE that touches its controllers, the
routes each controller declares at the commit's parent and at the commit are read from source (`routes`, ASP.NET's
attribute routing: a class's `[Route]` prefix, `[controller]` and `[action]`, each verb attribute's template, a
template starting `/` taken whole). A route the parent serves and the commit doesn't (by verb and path, the path
compared without case and with each parameter written `{}`) is removed or, when its action still serves another
route, changed. The client is read at its last commit at or before the server commit's committer date. Its requests
for a route are found by the adapter for its client APIs (`jellyfin_requests`): a call of the generated SDK's method
for the action (the operation id, the action's name in lower camel case, in a file importing `@jellyfin/sdk`) and a
URL built for `getUrl` whose path, with each value written `{}`, is the route's. A request is a label for a route
when it names it so; a call of a jellyfin-apiclient method (the library at the version the client's lock file installs,
read from its own repository's tag) is a request for each route the method's `getUrl` builds.

No repository code runs, and no model or KnowCode is asked anything.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from task_labels import GIT_ENV, git  # noqa: E402
import subprocess  # noqa: E402

SINCE = '2020-01-01'
PAIRS = {
    'jellyfin': dict(server='jellyfin-server', client='jellyfin-web', server_pin='aec42b345e1a7da25ee5492183f12209338df688',
                     client_pin='f21682ad693f87f513f578e2b3da6cc96271e76c', controllers=r'^Jellyfin\.Api/Controllers/.*\.cs$',
                     client_src='src/', requests='jellyfin'),
    'bitwarden': dict(server='bitwarden-server', client='bitwarden-clients', server_pin='9129efbd1531d5d8472345638efad7ef25a7337a',
                      client_pin='abccf1e342dccba06a3cc635cd4c706defa64a32', controllers=r'^src/.*/Controllers?/.*\.cs$',
                      client_src='libs/', requests='bitwarden'),
}
VERBS = {'HttpGet': 'GET', 'HttpPost': 'POST', 'HttpPut': 'PUT', 'HttpDelete': 'DELETE', 'HttpPatch': 'PATCH'}
ATTR = re.compile(r'\[\s*(Route|Http(?:Get|Post|Put|Delete|Patch))\s*(?:\(\s*(?:"([^"]*)")?[^)]*\))?\s*(?:,[^\]]*)?\]')
CLASS = re.compile(r'\bclass\s+(\w+)(?:\s*:\s*(\w+))?')
METHOD = re.compile(r'\bpublic\s+(?:async\s+|virtual\s+|override\s+|static\s+)*[\w<>\[\],.? ]+?\s+(\w+)\s*\(')
PARAM = re.compile(r'\{[^}]*\}')


def norm(path):
    """A path as ASP.NET compares it: no leading or trailing `/`, no case, each parameter `{}`."""
    return PARAM.sub('{}', path.strip('/')).lower()


def routes(text, bases=None):
    """The routes a controller file declares: (verb, path, action, line) for each verb attribute on a public method,
    under its class's `[Route]` prefix, or, when it writes none, its base class's (`bases`: class name to template),
    as ASP.NET inherits it."""
    out, prefix, controller, pending = [], '', '', []
    for n, line in enumerate(text.splitlines(), 1):
        attrs = ATTR.findall(line)
        c = CLASS.search(line)
        if c:
            controller = c.group(1)
            # The class's own `[Route]`, written on the lines above it.
            prefix = next((t for kind, t in pending if kind == 'Route'), None)
            if prefix is None:
                prefix = (bases or {}).get(c.group(2) or '', '')
            pending = []
            continue
        if attrs:
            pending += [(kind, t) for kind, t in attrs]
            continue
        m = METHOD.search(line)
        if m and pending:
            action = m.group(1)
            for kind, t in pending:
                if kind not in VERBS:
                    continue
                path = t if t.startswith('/') else '/'.join(p for p in (prefix, t) if p)
                name = controller[:-len('Controller')] if controller.endswith('Controller') else controller
                path = path.replace('[controller]', name).replace('[action]', action)
                out.append((VERBS[kind], path.strip('/'), action, n))
            pending = []
        elif line.strip() and not line.strip().startswith(('//', '///')):
            pending = [] if m else pending
    return out


def base_prefixes(clone, sha):
    """The `[Route]` templates of the base controllers at `sha` (`Base*Controller.cs`): what a controller writing no
    `[Route]` of its own inherits."""
    out = {}
    for f in git(clone, 'ls-tree', '-r', '--name-only', sha).splitlines():
        if re.search(r'(^|/)Base\w*Controller\.cs$', f):
            pending = []
            for line in git(clone, 'show', f'{sha}:{f}').splitlines():
                pending += [(k, t) for k, t in ATTR.findall(line)]
                c = CLASS.search(line)
                if c:
                    route = next((t for k, t in pending if k == 'Route'), None)
                    if route is not None:
                        out[c.group(1)] = route
                    pending = []
    return out


def server_routes(clone, sha, controllers):
    """{(verb, norm path): (path, action, file, line)} of every controller at `sha`."""
    out = {}
    bases = base_prefixes(clone, sha)
    files = [f for f in git(clone, 'ls-tree', '-r', '--name-only', sha).splitlines() if re.search(controllers, f)]
    for f in files:
        for verb, path, action, line in routes(git(clone, 'show', f'{sha}:{f}'), bases):
            out.setdefault((verb, norm(path)), (path, action, f, line))
    return out


def lower_camel(name):
    return name[:1].lower() + name[1:]


GETURL = re.compile(r'getUrl\(\s*')


def url_template(src, at):
    """The path a `getUrl(` call at `at` builds, each value written `{}`: its string literals joined, the expressions
    between them `{}`; `None` when it starts with no literal."""
    i, parts, depth = at, [], 0
    while i < len(src):
        ch = src[i]
        if ch in '\'"`':
            j = src.find(ch, i + 1)
            if j < 0:
                return None
            lit = src[i + 1:j]
            if ch == '`':
                lit = re.sub(r'\$\{[^}]*\}', '{}', lit)
            parts.append(lit)
            i = j + 1
            continue
        if ch == '(':
            depth += 1
        elif ch == ')':
            if depth == 0:
                break
            depth -= 1
        elif ch == ',' and depth == 0:
            break
        elif ch == '+' or ch.isspace():
            pass
        elif not parts or parts[-1] != '{}':
            parts.append('{}')
        i += 1
    if not parts or parts[0] == '{}':
        return None
    return ''.join(parts)


def jellyfin_requests(clone, sha, src_dir):
    """Every request the client at `sha` makes that names a route: (kind, key, file, line), `sdk` with the method called,
    `url` with the path built. Read with `git grep` at the commit, never from a checkout."""
    def grep(*args):
        p = git_ok(clone, 'grep', '-n', '-I', *args, sha, '--', src_dir)
        for l in p.splitlines():
            _, f, n, text = l.split(':', 3)
            if f.endswith(('.ts', '.tsx', '.js', '.jsx')) and '/__tests__/' not in f and not re.search(r'\.(test|spec)\.', f):
                yield f, int(n), text
    out = []
    sdk_files = {l.split(':', 1)[1] for l in git_ok(clone, 'grep', '-l', '-F', '@jellyfin/sdk', sha, '--', src_dir).splitlines()}
    # `git grep` takes POSIX patterns, without `\s` or `\w`: a fixed string finds the lines, Python's regex the calls.
    for f, n, line in grep('-F', '-e', ')'):
        if f in sdk_files:
            for m in re.finditer(r'\)\s*\.\s*(\w+)\s*\(', line):
                out.append(('sdk', m.group(1), f, n))
    # Calls of jellyfin-apiclient's methods (at the version the client installs): the routes they request.
    library = Path(clone).parent/APICLIENT
    version = apiclient_version(clone, sha) if library.is_dir() else None
    methods = apiclient_methods(library, version) if version else {}
    if methods:
        for f, n, line in grep('-i', '-F', 'apiclient'):
            for m in re.finditer(r'[Aa]pi[Cc]lient\)?\s*\.\s*(\w+)\s*\(', line):
                for verb, path in methods.get(m.group(1), []):
                    out.append(('wrap', f'{verb} {path}', f, n))
    for f, n, line in grep('-F', 'getUrl('):
        for m in GETURL.finditer(line):
            t = url_template(line, m.end())
            if t:
                out.append(('url', norm(t.split('?')[0]), f, n))
    return out


APICLIENT = 'jellyfin-apiclient'
METHOD_DEF = re.compile(r'^    (?:async\s+)?(\w+)\s*\(([^)]*)\)\s*\{', re.M)
AJAX_TYPE = re.compile(r"""type:\s*['"](GET|POST|PUT|DELETE|PATCH)['"]""")


def apiclient_version(clone, sha):
    """The jellyfin-apiclient version the client at `sha` installs: its lock file's, else its manifest's, `^` dropped."""
    names = git_ok(clone, 'ls-tree', '--name-only', sha).split()
    for lock in ('package-lock.json', 'yarn.lock'):
        if lock not in names:
            continue
        text = git_ok(clone, 'show', f'{sha}:{lock}')
        m = re.search(r'"node_modules/jellyfin-apiclient":\s*\{\s*"version":\s*"([\d.]+)"', text) or \
            re.search(r'"jellyfin-apiclient":\s*\{\s*"version":\s*"([\d.]+)"', text) or \
            re.search(r'^"?jellyfin-apiclient@[^\n]*:\n\s+version "([\d.]+)"', text, re.M)
        if m:
            return m.group(1)
    m = re.search(r'"jellyfin-apiclient":\s*"[\^~]?([\d.]+)"', git_ok(clone, 'show', f'{sha}:package.json'))
    return m.group(1) if m else None


def apiclient_methods(clone, version, cache={}):
    """{method: [(verb, path)]} of jellyfin-apiclient at the tag `v{version}`: each method's `getUrl` paths, with each
    value `{}`, and its verb (an `ajax` call's `type`, else GET)."""
    if (clone, version) in cache:
        return cache[(clone, version)]
    try:
        text = git_ok(clone, 'show', f'v{version}:src/apiClient.js')
    except RuntimeError:
        text = ''
    out = {}
    starts = [(m.start(), m.group(1)) for m in METHOD_DEF.finditer(text)]
    for i, (at, name) in enumerate(starts):
        body = text[at:starts[i + 1][0] if i + 1 < len(starts) else len(text)]
        typed = AJAX_TYPE.search(body)
        verb = typed.group(1) if typed else 'GET'
        for m in GETURL.finditer(body):
            t = url_template(body, m.end())
            if t:
                out.setdefault(name, []).append((verb, norm(t.split('?')[0])))
    cache[(clone, version)] = out
    return out


def git_ok(clone, *args):
    """`git` whose exit status 1 (`grep` matching nothing) is an empty answer."""
    p = subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', *args], cwd=clone, env=GIT_ENV, capture_output=True, text=True)
    if p.returncode not in (0, 1):
        raise RuntimeError(p.stderr)
    return p.stdout


def consumers(route_key, action, requests):
    """The requests naming the route: the SDK method for its action, a URL of its path (`url`: any verb, the verb
    being chosen elsewhere in the call), or a verb and URL of its verb and path (`send`)."""
    verb, path = route_key
    return [dict(file=f, line=n, by=kind) for kind, key, f, n in requests
            if (kind == 'sdk' and key == lower_camel(action)) or (kind == 'url' and key == path)
            or (kind in ('send', 'wrap') and key == f'{verb} {path}')]


SEND = re.compile(r'\.send\(\s*["\'](GET|POST|PUT|DELETE|PATCH)["\']\s*,\s*', re.S)


def bitwarden_requests(clone, sha, src_dir):
    """Every `ApiService.send(verb, path, …)` call of the client at `sha` whose path starts with a literal: (`send`,
    `VERB path`, file, line)."""
    out = []
    files = [l.split(':', 1)[1] for l in git_ok(clone, 'grep', '-l', '-F', '.send(', sha, '--', src_dir).splitlines()]
    for f in files:
        if not f.endswith('.ts') or re.search(r'\.(spec|test)\.ts$', f):
            continue
        text = git_ok(clone, 'show', f'{sha}:{f}')
        for m in SEND.finditer(text):
            t = url_template(text, m.end())
            if t:
                line = text.count('\n', 0, m.start()) + 1
                out.append(('send', f"{m.group(1)} {norm(t.split('?')[0])}", f, line))
    return out


REQUESTS = {'jellyfin': jellyfin_requests, 'bitwarden': bitwarden_requests}


def survey(work, pair):
    p = PAIRS[pair]
    server, client = Path(work)/'clones'/p['server'], Path(work)/'clones'/p['client']
    # The commits touching a controller (by the pathspec of the directories controllers are in; filtered by regex after).
    log = git(server, 'log', '--no-merges', f'--since={SINCE}', '--format=%H %cI', p['server_pin'], '--', '*Controller*/*.cs')
    rows, requests = [], {}
    for entry in log.splitlines():
        sha, when = entry.split()
        parents = git(server, 'rev-list', '--parents', '-n', '1', sha).split()[1:]
        if len(parents) != 1:
            continue
        changed = [f for f in git(server, 'diff-tree', '--no-commit-id', '--name-only', '-r', parents[0], sha).splitlines()
                   if re.search(p['controllers'], f)]
        if not changed:
            continue
        def of(rev, files):
            out = {}
            bases = base_prefixes(server, rev)
            for f in files:
                try:
                    text = git_ok(server, 'show', f'{rev}:{f}')
                except RuntimeError:
                    continue  # a file the commit adds, or deletes, isn't there on one side
                for verb, path, action, line in routes(text, bases):
                    out.setdefault((verb, norm(path)), (path, action, f, line))
            return out
        before = of(parents[0], changed)
        gone = {k: v for k, v in before.items() if k not in of(sha, changed)}
        if not gone:
            continue
        # A route moved to another controller is still served.
        after = server_routes(server, sha, p['controllers'])
        gone = {k: v for k, v in gone.items() if k not in after}
        if not gone:
            continue
        csha = git(client, 'log', '-1', f'--before={when}', '--format=%H', p['client_pin']).strip()
        if csha and csha not in requests:
            requests[csha] = REQUESTS[p['requests']](client, csha, p['client_src'])
        reqs = requests.get(csha, [])
        actions_now = {v[1] for v in after.values()}
        for (verb, path), (written, action, f, line) in sorted(gone.items()):
            rows.append(dict(pair=pair, commit=sha, parent=parents[0], client_commit=csha, verb=verb, path=written, action=action,
                             file=f, line=line, change='changed' if action in actions_now else 'removed',
                             consumers=consumers((verb, path), action, reqs)))
        print(f'{sha[:10]}: {len(gone)} routes gone', file=sys.stderr, flush=True)
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    s = sub.add_parser('survey')
    s.add_argument('--work', required=True)
    s.add_argument('--pair', default='jellyfin', choices=sorted(PAIRS))
    s.add_argument('--out')
    args = ap.parse_args()
    rows = survey(args.work, args.pair)
    if args.out:
        Path(args.out).write_text(''.join(json.dumps(r, sort_keys=True) + '\n' for r in rows))
    commits = {r['commit'] for r in rows}
    with_consumers = {r['commit'] for r in rows if r['consumers']}
    print(f'{len(rows)} routes removed or changed in {len(commits)} commits; {sum(1 for r in rows if r["consumers"])} routes '
          f'with a consumer, in {len(with_consumers)} commits')


if __name__ == '__main__':
    main()
