#!/usr/bin/env python3
"""route-links-v1: client call sites to label, and KnowCode's links scored against the labels.

    route_links.py sites CLONES REPOS.txt > sites.jsonl
    route_links.py seeded sites.jsonl > second-ids.txt
    route_links.py score --knowcode BIN --reader BIN --work WORK --repos REPOS.txt --labels LABELS [--out SUMMARY]

`sites` lists every client call site in the pinned clones by the search eval/route-links-v1/README.md states: one row
per request-API call (TypeScript and JavaScript, Java and Kotlin, Go), and one per method of a `@FeignClient` or
`@HttpExchange` interface. Lines are counted with LF, CR LF or a lone CR ending a line, and two sites on one line are
told apart as `#2` and on. Test code is left out. A labeller reads each site and writes its label by hand.

`seeded` lists the sites labelled a second time: the SECOND share of them, the lowest by the SHA-256 of their id.

`score` scores KnowCode's links from the labelled sites by eval/route-links-v1/README.md's "Scoring", which fixed it
before any scorer existed. Each group's clones (WORK/clones/GROUP/REPO) are indexed into the build's own cache under
WORK/kc/<binary hash>/<group>, never in the clone, and its requests are read with the links of each through a reader
that isn't a product path (crates/kc-index/examples/client_links.rs): the `routes` answer names a request by the
symbol making it, and a site is a call, two of which one function can hold. The reader's links are the `routes`
answer's (both call `kc_index::http::handlers`); the scorer checks that per requesting symbol and reports the agreement.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import sys

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))  # scripts/: kc_env

SCRIPT_REQUEST = re.compile(
    r'(?<![\w$.])(?:this\.)?(?:http|httpClient)\.(?:get|post|put|patch|delete|request|head|options)\s*[<(]'
    r'|(?<![\w$.])fetch\('
    r'|(?<![\w$.])axios(?:\.(?:get|post|put|patch|delete|request|head|options))?\('
    r'|(?<![\w$])\$\.(?:ajax|get|post|getJSON)\('
    r'|(?<![\w$])\$http(?:\.(?:get|post|put|patch|delete|head|jsonp))?\('
    r'|(?<![\w$])\$resource\('
    r'|(?<![\w$.])httpResource\s*[<(]')
# The rest of an HttpClient call chained across lines: `this.http` ends one line, `.put<…>(` starts the next.
CHAINED = re.compile(r'^\s*\.(?:get|post|put|patch|delete|request|head|options)\s*[<(]')
CHAIN_HEAD = re.compile(r'(?<![\w$.])(?:this\.)?(?:http|httpClient)\s*$')
# A declaration named like a request API (`private fetch(): Observable<…>`), not a call.
DECLARED = re.compile(r'\b(?:private|public|protected|static|async|function)\s*$')
# Code a tool generated and the repository commits (eval/route-links-v1/README.md).
EXCLUDED = {
    'spring-petclinic-angular': ('docs/',),
    # eval/route-links-go-v1/README.md: bundled third-party scripts, and the server's API test helpers.
    'sftpgo': ('openapi/swagger-ui/', 'static/assets/js/scripts.bundle.js', 'internal/httpdtest/'),
}
# A Go file a tool generated says so in its first lines, by Go's convention (`go help generate`).
GENERATED_GO = re.compile(r'^// Code generated .* DO NOT EDIT\.$')
# A Spring client's request: the one `uri(…)` of a WebClient or RestClient request, or a RestTemplate request method.
JVM_REQUEST = re.compile(
    r'\.uri\('
    r'|\.(?:getForObject|getForEntity|postForObject|postForEntity|postForLocation|patchForObject|exchange)\('
    r'|(?i:resttemplate)\w*\.(?:put|delete|execute)\(')
# Go: net/http's request functions, and `Do` on an http.Client (a value named for one: `client`, `c.HTTPClient`).
GO_REQUEST = re.compile(
    r'(?<![\w.])http\.(?:Get|Post|PostForm|Head|NewRequest|NewRequestWithContext)\('
    r'|(?<![\w])(?:http\.DefaultClient|\w*(?i:client))\.Do\(')
DECLARATIVE_CLIENT = re.compile(r'@(?:FeignClient|HttpExchange)\b')
MAPPING = re.compile(r'@(?:RequestMapping|GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping|'
                     r'HttpExchange|GetExchange|PostExchange|PutExchange|PatchExchange|DeleteExchange)\b')
METHOD_NAME = re.compile(r'\b([A-Za-z_]\w*)\s*\(')
SECOND = 0.2
SCRIPTS = ('.ts', '.tsx', '.js', '.jsx')
JVM = ('.java', '.kt')
GO = ('.go',)
TEST = re.compile(r'(^|/)(test|tests|__tests__|e2e|cypress)(/|$)|\.(spec|test)\.[jt]sx?$|Test\.(java|kt)$|_test\.go$')
SKIP = re.compile(r'(^|/)(node_modules|vendor|lib|dist|build|target)(/|$)|\.min\.js$')


def lines(path):
    """The file's lines, ended by LF, CR LF or a lone CR."""
    return path.read_bytes().decode('utf-8', 'replace').splitlines()


def comment(text, at):
    """Whether position `at` of a line is in a `//` comment (a rough test; block comments aren't tracked)."""
    start = text.find('//')
    return 0 <= start < at and not text[:start].rstrip().endswith(':')


def in_string(text, at):
    """Whether position `at` of a Go line is inside a string literal (generated code's text, not a call)."""
    quotes = 0
    i = 0
    while i < at:
        if text[i] == '\\':
            i += 2
            continue
        if text[i] == '"':
            quotes += 1
        i += 1
    return quotes % 2 == 1


def declarative_sites(rel, text):
    """A site per mapped method of a @FeignClient or @HttpExchange interface: its name's line."""
    out, pending = [], False
    for n, line in enumerate(text, 1):
        if MAPPING.search(line):
            pending = True
        if pending:
            m = METHOD_NAME.search(MAPPING.sub('', line))
            if m and not line.lstrip().startswith('@') and m.group(1) not in ('value', 'path', 'method'):
                out.append((n, 1, 'declarative', line.strip()))
                pending = False
    return out


def sites(clone, repo):
    files = subprocess.run(['git', '-C', str(clone), 'ls-files', '-z'], check=True, capture_output=True,
                           text=True).stdout.split('\0')
    out = []
    for rel in sorted(f for f in files if f):
        if TEST.search(rel) or SKIP.search(rel) or not rel.endswith(SCRIPTS + JVM + GO) \
                or rel.startswith(EXCLUDED.get(repo, ())):
            continue
        text = lines(Path(clone)/rel)
        if rel.endswith(GO) and any(GENERATED_GO.match(t) for t in text[:20]):
            continue
        if rel.endswith(JVM) and any(DECLARATIVE_CLIENT.search(t) for t in text):
            found = declarative_sites(rel, text)
        else:
            pattern = SCRIPT_REQUEST if rel.endswith(SCRIPTS) else GO_REQUEST if rel.endswith(GO) else JVM_REQUEST
            found = [(n, m.start() + 1, m.group(0), t.strip()) for n, t in enumerate(text, 1)
                     for m in pattern.finditer(t)
                     if not comment(t, m.start()) and not DECLARED.search(t[:m.start()])
                     and not (rel.endswith(GO) and in_string(t, m.start()))]
            if rel.endswith(SCRIPTS):
                found += [(n, m.start() + 1, m.group(0).strip(), t.strip()) for n, t in enumerate(text, 1)
                          if n > 1 and CHAIN_HEAD.search(text[n - 2]) for m in [CHAINED.match(t)] if m]
                found.sort()
        per_line = {}
        for n, col, api, context in found:
            per_line[n] = per_line.get(n, 0) + 1
            suffix = f'#{per_line[n]}' if per_line[n] > 1 else ''
            out.append(dict(id=f'{repo}/{rel}:{n}{suffix}', repo=repo, file=rel, line=n, col=col, api=api,
                            context=context[:200]))
    return out


def seeded(ids):
    """The ids labelled twice: ceil(SECOND × n) of them, lowest by SHA-256, so the choice depends on the ids alone."""
    ids = sorted(set(ids))
    return sorted(sorted(ids, key=lambda i: hashlib.sha256(i.encode()).hexdigest())[:math.ceil(SECOND * len(ids))])


LANGUAGES = {'Go': GO, 'TypeScript and JavaScript': SCRIPTS + ('.vue',), 'Java and Kotlin': JVM, 'C#': ('.cs',)}
SITE_ID = re.compile(r'^([^/]+)/(.+):(\d+)(?:#\d+)?$')
EVIDENCE = re.compile(r'^([^/]+)/(.+):(\d+)$')
NEAR = 3


def language(file):
    return next((name for name, ext in LANGUAGES.items() if file.endswith(ext)), 'other')


def route_key(verb, path):
    """A route by LABELING.md's Routes ruling: case ignored, leading and trailing slashes dropped, `{param}` and
    `:param` alike (and the query string left off)."""
    p = re.sub(r'\{[^}]*\}|:[A-Za-z_]\w*', '{}', (path or '').split('?', 1)[0]).strip('/').lower()
    return (verb or '').upper(), p


def same_route(a, b):
    """Two routes are equal by the ruling; a route answering every verb (`*`) answers each."""
    return (a[0] == b[0] or '*' in (a[0], b[0])) and a[1] == b[1]


def item_route(item):
    verb, _, path = item.strip().partition(' ')
    return route_key(verb, path)


def match(link, target):
    """How a link matches a label's item or negative, or None: `file` when the linked handler is in the item's file
    within NEAR lines of its evidence, `route` when the linked route equals the item's."""
    m = EVIDENCE.match(target.get('evidence', ''))
    if m and link['repo'] == m.group(1) and link['file'] == m.group(2) \
            and abs(link['start_line'] - int(m.group(3))) <= NEAR:
        return 'file'
    if link.get('path') is not None and same_route(route_key(link['verb'], link['path']), item_route(target['item'])):
        return 'route'
    return None


def links_at(rows, label):
    """KnowCode's links from a site: those of every request the reader places at the site's file and line, once each."""
    repo, file, line = SITE_ID.match(label['id']).groups()
    out = {}
    for r in rows:
        if r['repo'] == repo and r['file'] == file and r['line'] == int(line):
            for l in r['links']:
                out[(l['repo'], l['file'], l['line'], l['verb'], l.get('path'))] = l
    return [out[k] for k in sorted(out, key=str)]


def tally(labels, rows):
    """Per site, by the rule: precision over the links from `exhaustive` sites (a link counts right when it matches one
    of the site's items and none of its negatives; a link from a site with no items is wrong); recall over the
    required items of `exhaustive` and `lower_bound` sites, with an optional item counted only when it is linked.
    `uncertain` sites are kept apart."""
    out = []
    for label in labels:
        links = links_at(rows, label)
        site = dict(id=label['id'], group=label['group'], language=language(SITE_ID.match(label['id']).group(2)),
                    dynamic=bool(label.get('dynamic_name')), completeness=label['completeness'],
                    links=len(links), right=0, wrong=[], required=0, found=0, missed=[], clauses={})
        for l in links:
            how = [match(l, i) for i in label['items']]
            bad = any(match(l, n) for n in label.get('negatives', []))
            if any(how) and not bad:
                site['right'] += 1
                c = next(h for h in how if h)
                site['clauses'][c] = site['clauses'].get(c, 0) + 1
            else:
                site['wrong'].append(f"{l['verb']} /{l.get('path') or ''} at {l['repo']}/{l['file']}:{l['line']}")
        for i in label['items']:
            linked = any(match(l, i) for l in links)
            if i.get('optional') and not linked:
                continue
            site['required'] += 1
            site['found'] += linked
            if not linked:
                site['missed'].append(i['item'])
        out.append(site)
    return out


def figures(sites):
    """Precision and recall with counts and Wilson 95% intervals over some sites."""
    from common import wilson
    prec = [s for s in sites if s['completeness'] == 'exhaustive']
    rec = [s for s in sites if s['completeness'] in ('exhaustive', 'lower_bound')]
    k, n = sum(s['right'] for s in prec), sum(s['links'] for s in prec)
    rk, rn = sum(s['found'] for s in rec), sum(s['required'] for s in rec)
    return dict(sites=len(sites), uncertain=sum(s['completeness'] == 'uncertain' for s in sites),
                precision=dict(k=k, n=n, ci=[round(x, 3) for x in wilson(k, n)]),
                recall=dict(k=rk, n=rn, ci=[round(x, 3) for x in wilson(rk, rn)]))


def breakdown(sites):
    """The rule's figures: per client language and per repository group, with and without the dynamic_name sites."""
    out = {}
    for by in ('language', 'group'):
        for key in sorted({s[by] for s in sites}):
            mine = [s for s in sites if s[by] == key]
            out[f'{by}:{key}'] = dict(all=figures(mine), without_dynamic=figures([s for s in mine if not s['dynamic']]))
    out['all'] = dict(all=figures(sites), without_dynamic=figures([s for s in sites if not s['dynamic']]))
    return out


def fmt(f):
    if not f['n']:
        return '–'
    return f"{f['k'] / f['n']:.3f} ({f['k']}/{f['n']}, {f['ci'][0]:.3f}–{f['ci'][1]:.3f})"


def report(summary):
    lines = ['| Slice | Sites | Precision | Recall | Precision, no dynamic_name | Recall, no dynamic_name |',
             '| --- | ---: | --- | --- | --- | --- |']
    for key, v in summary['figures'].items():
        a, d = v['all'], v['without_dynamic']
        lines.append(f"| {key} | {a['sites']} | {fmt(a['precision'])} | {fmt(a['recall'])} | {fmt(d['precision'])} "
                     f"| {fmt(d['recall'])} |")
    return '\n'.join(lines)


def read_answers(binary, env, clones):
    """The `routes` answer's links per requesting symbol: {(repo, symbol): {(repo, file, line)}}."""
    args = [str(binary), 'routes', '--json', '--no-sync']
    for c in clones:
        args += ['--repo', str(c)]
    out = json.loads(subprocess.run(args, env=env, cwd=clones[0], check=True, capture_output=True, text=True).stdout)
    got = {}
    for item in out['items']:
        got.setdefault((item['repo'], item['symbol']), set()).update(
            (l['repo'], l['file'], l['line']) for l in item.get('links', []))
    return got


def run(args):
    import kc_env
    labels = [json.loads(x) for x in Path(args.labels).read_text().splitlines() if x.strip()]
    groups = {}
    for line in Path(args.repos).read_text().splitlines():
        if line.strip() and not line.startswith('#'):
            group, repo = line.split()[:2]
            groups.setdefault(group, []).append(repo)
    tag = hashlib.sha256(Path(args.knowcode).read_bytes()).hexdigest()[:16]
    rows, agree = [], dict(symbols=0, same=0, differ=[])
    for group, repos in groups.items():
        clones = [Path(args.work)/'clones'/group/r for r in repos]
        env = kc_env.knowcode_env(Path(args.work)/'kc'/tag/group)
        for c in clones:
            subprocess.run([str(args.knowcode), 'index', str(c)], env=env, cwd=c, check=True, capture_output=True)
        mine = [json.loads(x) for x in subprocess.run([str(args.reader), *map(str, clones)], env=env, cwd=clones[0],
                                                     check=True, capture_output=True, text=True).stdout.splitlines()]
        rows += mine
        answers = read_answers(args.knowcode, env, clones)
        per_symbol = {}
        for r in mine:
            per_symbol.setdefault((r['repo'], r['symbol']), set()).update(
                (l['repo'], l['file'], l['line']) for l in r['links'])
        for key in sorted(set(per_symbol) | set(answers)):
            agree['symbols'] += 1
            if per_symbol.get(key, set()) == answers.get(key, set()):
                agree['same'] += 1
            else:
                agree['differ'].append(f'{key[0]}: {key[1]}')
    sites = tally(labels, rows)
    summary = dict(rule='route-links-v1', binary=tag, labels=args.labels, agreement=agree,
                   figures=breakdown(sites), sites=sites)
    if args.out:
        Path(args.out).write_text(json.dumps(summary, indent=1, sort_keys=True) + '\n')
    print(report(summary))
    print(f"\nreader and `routes` answer agree on {agree['same']} of {agree['symbols']} requesting symbols")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    s = sub.add_parser('sites')
    s.add_argument('clones')
    s.add_argument('repos')
    d = sub.add_parser('seeded')
    d.add_argument('sites')
    r = sub.add_parser('score')
    r.add_argument('--knowcode', required=True, help='the knowcode binary scored')
    r.add_argument('--reader', required=True, help="kc-index's client_links example, built from the same tree")
    r.add_argument('--work', required=True, help='WORK/clones/GROUP/REPO holds the pinned clones')
    r.add_argument('--repos', required=True, help="the set's repos.txt: group, repository, URL, commit")
    r.add_argument('--labels', required=True)
    r.add_argument('--out')
    args = ap.parse_args()
    if args.cmd == 'score':
        run(args)
        return
    if args.cmd == 'seeded':
        ids = [json.loads(line)['id'] for line in Path(args.sites).read_text().splitlines() if line.strip()]
        print('\n'.join(seeded(ids)))
        return
    if args.cmd == 'sites':
        for line in Path(args.repos).read_text().splitlines():
            if line.strip() and not line.startswith('#'):
                group, repo = line.split()[:2]
                for row in sites(Path(args.clones)/repo, repo):
                    sys.stdout.write(json.dumps(dict(row, group=group), sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
