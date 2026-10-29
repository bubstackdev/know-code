#!/usr/bin/env python3
"""route-links-v1: client call sites to label, and KnowCode's links scored against the labels.

    route_links.py sites CLONES REPOS.txt > sites.jsonl
    route_links.py seeded sites.jsonl > second-ids.txt

`sites` lists every client call site in the pinned clones by the search eval/route-links-v1/README.md states: one row
per request-API call (TypeScript and JavaScript, Java and Kotlin), and one per method of a `@FeignClient` or
`@HttpExchange` interface. Lines are counted with LF, CR LF or a lone CR ending a line, and two sites on one line are
told apart as `#2` and on. Test code is left out. A labeller reads each site and writes its label by hand.

`seeded` lists the sites labelled a second time: the SECOND share of them, the lowest by the SHA-256 of their id.
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
EXCLUDED = {'spring-petclinic-angular': ('docs/',)}
# A Spring client's request: the one `uri(…)` of a WebClient or RestClient request, or a RestTemplate request method.
JVM_REQUEST = re.compile(
    r'\.uri\('
    r'|\.(?:getForObject|getForEntity|postForObject|postForEntity|postForLocation|patchForObject|exchange)\('
    r'|(?i:resttemplate)\w*\.(?:put|delete|execute)\(')
DECLARATIVE_CLIENT = re.compile(r'@(?:FeignClient|HttpExchange)\b')
MAPPING = re.compile(r'@(?:RequestMapping|GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping|'
                     r'HttpExchange|GetExchange|PostExchange|PutExchange|PatchExchange|DeleteExchange)\b')
METHOD_NAME = re.compile(r'\b([A-Za-z_]\w*)\s*\(')
SECOND = 0.2
SCRIPTS = ('.ts', '.tsx', '.js', '.jsx')
JVM = ('.java', '.kt')
TEST = re.compile(r'(^|/)(test|tests|__tests__|e2e|cypress)(/|$)|\.(spec|test)\.[jt]sx?$|Test\.(java|kt)$')
SKIP = re.compile(r'(^|/)(node_modules|vendor|lib|dist|build|target)(/|$)|\.min\.js$')


def lines(path):
    """The file's lines, ended by LF, CR LF or a lone CR."""
    return path.read_bytes().decode('utf-8', 'replace').splitlines()


def comment(text, at):
    """Whether position `at` of a line is in a `//` comment (a rough test; block comments aren't tracked)."""
    start = text.find('//')
    return 0 <= start < at and not text[:start].rstrip().endswith(':')


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
        if TEST.search(rel) or SKIP.search(rel) or not rel.endswith(SCRIPTS + JVM) \
                or rel.startswith(EXCLUDED.get(repo, ())):
            continue
        text = lines(Path(clone)/rel)
        if rel.endswith(JVM) and any(DECLARATIVE_CLIENT.search(t) for t in text):
            found = declarative_sites(rel, text)
        else:
            pattern = SCRIPT_REQUEST if rel.endswith(SCRIPTS) else JVM_REQUEST
            found = [(n, m.start() + 1, m.group(0), t.strip()) for n, t in enumerate(text, 1)
                     for m in pattern.finditer(t)
                     if not comment(t, m.start()) and not DECLARED.search(t[:m.start()])]
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


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    s = sub.add_parser('sites')
    s.add_argument('clones')
    s.add_argument('repos')
    d = sub.add_parser('seeded')
    d.add_argument('sites')
    args = ap.parse_args()
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
