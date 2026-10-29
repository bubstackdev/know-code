"""compare-v1's runner (eval/compare-v1/README.md, "Asking" and "Scoring").

In the container, through container.sh:

    compare-python -m compare.run index --language L --repo R --src DIR --out OUT     # network up: each tool indexes
    compare-python -m compare.run ask --language L --repo R --src DIR --out OUT [--types refs,impact]   # network cut

On the host, over every repository's OUT and the pinned clones:

    python3 -m compare.run score --questions eval/compare-v1/questions.jsonl --clones CLONES --out SUMMARY.json OUT [...]

`ask` writes OUT/answers.jsonl, one line per question and tool: the outcome, the normalised items and every raw answer.
`score` judges them with score.py and writes the summary, with every miss listed.
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import sys
import time
import traceback

from compare import adapters, score, tools

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))  # scripts/eval: task_gate's judging

STATE = '/work/state'
QUESTIONS = '/in/questions.jsonl'
LANGUAGES = {'swift-argument-parser': 'swift', 'swift-collections': 'swift', 'swift-nio': 'swift',
             'serilog': 'csharp', 'autofac': 'csharp', 'jellyfin-server': 'csharp',
             'jellyfin-web': 'typescript', 'hono': 'typescript'}


def asked_tools(args):
    """The tools this run starts. A tool left out must have no call for any question type asked (mapping.toml): it
    answers `no_counterpart` without a session, which saves indexing a tree it will never be asked about."""
    return [t for t in tools.TOOLS if t in args.tools.split(',')]


def index(args):
    out = Path(args.out)
    record = {}
    for tool in asked_tools(args):
        started = time.time()
        try:
            session = tools.Session(tool, args.src, args.language, STATE, out/'logs')
            record[tool] = {'ok': True, 'project': session.project, 'server': session.client.server,
                            'seconds': round(time.time() - started, 1)}
            session.close()
        except Exception:  # noqa: BLE001 - a tool that can't index answers every question `error`
            record[tool] = {'ok': False, 'error': traceback.format_exc()[-4000:]}
    (out/'index.json').write_text(json.dumps(record, indent=1, sort_keys=True))


def ask(args):
    out = Path(args.out)
    indexed = json.loads((out/'index.json').read_text())
    types = set(args.types.split(','))
    from compare import questions as draw
    asked = draw.expand([json.loads(l) for l in Path(args.questions).read_text().splitlines() if l.strip()])
    questions = [q for q in asked if q['repo'] == args.repo and q['type'] in types]
    with open(out/'answers.jsonl', 'w') as f:
        for tool in tools.TOOLS:
            if tool not in indexed:
                for q in questions:
                    f.write(json.dumps({'id': q['id'], 'tool': tool, 'type': q['type'], 'outcome': 'no_counterpart',
                                        'items': [], 'raw': []}, sort_keys=True) + '\n')
                continue
            session = None
            if indexed[tool]['ok']:
                try:
                    session = tools.Session(tool, args.src, args.language, STATE, out/'logs', setup=False,
                                            project=indexed[tool].get('project'))
                except Exception:  # noqa: BLE001
                    indexed[tool]['reopen_error'] = traceback.format_exc()[-4000:]
            for q in questions:
                if session is None:
                    outcome, items, raw = 'error', [], ['the tool did not index or reopen the repository']
                else:
                    try:
                        outcome, items, raw = adapters.ASK[tool](session, q['type'], q)
                    except Exception:  # noqa: BLE001 - an adapter failure is the tool's error, logged whole
                        outcome, items, raw = 'error', [], [traceback.format_exc()[-4000:]]
                f.write(json.dumps({'id': q['id'], 'tool': tool, 'type': q['type'], 'outcome': outcome,
                                    'items': items, 'raw': raw}, sort_keys=True) + '\n')
                f.flush()
            if session is not None:
                session.close()
    (out/'index.json').write_text(json.dumps(indexed, indent=1, sort_keys=True))


# ---------------------------------------------------------------------------------------------------------------------
# Scoring, on the host.

def enumeration_view(enumerations):
    """Per repository: declarations as (file, line, name), and per name the lines its reference rows are bound on."""
    from compare import questions as draw
    decls, bound = {}, {}
    for path in enumerations:
        rows = draw.read(path)
        repo = rows[0]['repo']
        decls[repo] = {(r['file'], r['line'], r['name']) for r in rows if r['t'] == 'decl'}
        by = defaultdict(lambda: defaultdict(set))
        for r in rows:
            if r['t'] == 'ref':
                by[r['name']][(r['file'], r['line'])].add(r['key'])
        bound[repo] = by
    return decls, bound


def judge(q, answer, decls, bound, source):
    items = [tuple(i) for i in answer['items']]
    if q['type'] == 'refs':
        labels = {tuple(s) for s in q['labels']}
        return score.judge_refs(items, labels, bound[q['repo']].get(q['target'][2], {}))
    labels = {tuple(d) for d in q['labels']}
    return score.judge_decls(items, labels, decls[q['repo']], source)


def summarise(questions, answers, decls, bound, clones):
    texts = {}

    def source(repo):
        def read(f):
            key = (repo, f)
            if key not in texts:
                p = Path(clones)/repo/f
                texts[key] = p.read_text(errors='replace').split('\n') if p.is_file() else None
            return texts[key]
        return read

    by_id = {q['id']: q for q in questions}
    groups = defaultdict(lambda: {'judged': [], 'found': [], 'outcomes': []})
    misses = []
    for a in answers:
        q = by_id[a['id']]
        lang = LANGUAGES[q['repo']]
        for key in ((a['tool'], q['type'], lang), (a['tool'], q['type'], 'all')):
            groups[key]['outcomes'].append(a['outcome'])
        if a['outcome'] == 'judged':
            j = judge(q, a, decls, bound, source(q['repo']))
            for key in ((a['tool'], q['type'], lang), (a['tool'], q['type'], 'all')):
                groups[key]['judged'].append(j)
            misses.append({'id': q['id'], 'tool': a['tool'], 'missed': j.misses, 'wrong': j.wrong,
                           'unjudged': len(j.unjudged)})
    # The second score (README, "Lookup"): only the questions the tool answered, with the other outcomes counted.
    out = [{'tool': tool, 'type': qtype, 'language': lang, 'outcomes': score.tally(g['outcomes']),
            **score.pool(g['judged'])} for (tool, qtype, lang), g in sorted(groups.items())]
    return out, misses


def language(q):
    return q.get('lang') or LANGUAGES[q['repo']]


def summarise_paths(questions, answers, budgets=(2000, 4000, 8000), misses=None):
    """locate and task (README, "Scoring"): per tool, type and language, recall of the changed files and, apart, of the
    changed tests, on the whole answer and within each byte budget, with the outcomes counted apart."""
    import task_gate
    by_id = {q['id']: q for q in questions}
    groups = defaultdict(lambda: {'outcomes': [], 'files': Counter(), 'tests': Counter(), 'questions': 0})
    for a in answers:
        q = by_id[a['id']]
        if q['type'] not in ('locate', 'task'):
            continue
        for key in ((a['tool'], q['type'], language(q)), (a['tool'], q['type'], 'all')):
            g = groups[key]
            g['outcomes'].append(a['outcome'])
            if a['outcome'] != 'judged':
                continue
            g['questions'] += 1
            text = '\n'.join(a['raw'])
            for kind in ('files', 'tests'):
                judged = task_gate.named(text, q[kind], budgets)
                for named in judged.values():
                    g[kind]['n'] += 1
                    for at in ('all', *map(str, budgets)):
                        g[kind][at] += named[at]
                if misses is not None and key[2] != 'all':
                    missed = [p for p, n in judged.items() if not n['all']]
                    if missed:
                        misses.append({'id': q['id'], 'tool': a['tool'], 'missed': [[p, '', kind] for p in missed],
                                       'wrong': [], 'unjudged': 0})
    out = []
    for (tool, qtype, lang), g in sorted(groups.items()):
        row = {'tool': tool, 'type': qtype, 'language': lang, 'questions': g['questions'],
               'outcomes': score.tally(g['outcomes'])}
        for kind in ('files', 'tests'):
            row[kind] = {at: score._rate(g[kind][at], g[kind]['n']) for at in ('all', *map(str, budgets))}
        out.append(row)
    return out


def score_command(args):
    from compare import questions as draw
    questions = draw.expand([json.loads(l) for l in Path(args.questions).read_text().splitlines() if l.strip()])
    answers = []
    for d in args.outs:
        answers += [json.loads(l) for l in (Path(d)/'answers.jsonl').read_text().splitlines() if l.strip()]
    decls, bound = enumeration_view(sorted(Path(args.enumerations).glob('enumeration-*.jsonl.gz')))
    symbol = [a for a in answers if a['type'] not in ('locate', 'task')]
    summary, misses = summarise(questions, symbol, decls, bound, args.clones)
    all_questions = score_all_questions(questions, symbol, decls, bound, args.clones)
    paths = summarise_paths(questions, answers, misses=misses)
    Path(args.out).write_text(json.dumps({'answered': summary, 'all_questions': all_questions, 'paths': paths},
                                         indent=1, sort_keys=True))
    with open(Path(args.out).with_suffix('.misses.jsonl'), 'w') as f:
        for m in misses:
            f.write(json.dumps(m, sort_keys=True) + '\n')


def score_all_questions(questions, answers, decls, bound, clones):
    """The first score (README, "Lookup"): every question, a `not_found` or `error` counted with all its labels missed.
    `no_counterpart` types are left out: the tool has no call for them."""
    by_id = {q['id']: q for q in questions}
    groups = defaultdict(list)
    texts = {}
    for a in answers:
        q = by_id[a['id']]
        if a['outcome'] == 'no_counterpart':
            continue
        if a['outcome'] == 'judged':
            def source(f, repo=q['repo']):
                key = (repo, f)
                if key not in texts:
                    p = Path(clones)/repo/f
                    texts[key] = p.read_text(errors='replace').split('\n') if p.is_file() else None
                return texts[key]
            j = judge(q, a, decls, bound, source)
        else:
            j = score.Judged(misses=[tuple(x) for x in q['labels']])
        for lang in (LANGUAGES[q['repo']], 'all'):
            groups[(a['tool'], q['type'], lang)].append(j)
    return [{'tool': t, 'type': ty, 'language': l, **score.pool(js)} for (t, ty, l), js in sorted(groups.items())]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='command', required=True)
    for name in ('index', 'ask'):
        p = sub.add_parser(name)
        p.add_argument('--language', required=True)
        p.add_argument('--repo', required=True)
        p.add_argument('--src', required=True)
        p.add_argument('--out', required=True)
        p.add_argument('--questions', default=QUESTIONS)
        p.add_argument('--types', default='refs,impact')
        p.add_argument('--tools', default=','.join(tools.TOOLS))
    p = sub.add_parser('score')
    p.add_argument('--questions', required=True)
    p.add_argument('--enumerations', required=True)
    p.add_argument('--clones', required=True)
    p.add_argument('--out', required=True)
    p.add_argument('outs', nargs='+')
    args = ap.parse_args(argv)
    {'index': index, 'ask': ask, 'score': score_command}[args.command](args)


if __name__ == '__main__':
    sys.exit(main())
