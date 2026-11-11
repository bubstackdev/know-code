"""compare-v1's published results (eval/compare-v1/results.md) from scored runs.

    python3 -m compare.report --out eval/compare-v1 --run first=RUN1,RUN1B --run rerun=RUN2,RUN2B [--note NAME=TEXT]

Each `--run NAME=A,B` names one published run and its two repetitions: directories holding run.py's summary.json,
summary.misses.jsonl and per-repository `*/out/answers.jsonl`. The first repetition's figures are published; the
second is compared with it answer by answer. Writes results.md and, per run, results/NAME.misses.tsv (every label
missed, every item judged wrong, and every question a tool couldn't answer, with its outcome).
"""
from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import json
from pathlib import Path
import re

TOOLS = ('knowcode', 'codebase-memory-mcp', 'serena')
NAMES = {'knowcode': 'KnowCode', 'codebase-memory-mcp': 'codebase-memory-mcp', 'serena': 'Serena'}
LANGS = ('csharp', 'swift', 'typescript', 'all')
LANG_NAMES = {'csharp': 'C#', 'swift': 'Swift', 'typescript': 'TypeScript', 'all': 'All'}


def answers(directory):
    out = {}
    for p in sorted(Path(directory).glob('*/out/answers.jsonl')):
        for line in p.read_text().splitlines():
            a = json.loads(line)
            out[(a['tool'], a['id'])] = a
    return out


def fmt(rate):
    if rate['rate'] is None:
        return '–'
    return f"{rate['rate']:.3f} ({rate['k']}/{rate['n']}; {rate['low']:.2f}–{rate['high']:.2f})"


def table(rows, kind):
    """Markdown rows for one score (answered or all_questions) per type, tool and language."""
    by = {(r['type'], r['tool'], r['language']): r for r in rows}
    out = []
    for qtype in [t for t in ('refs', 'callers', 'impact', 'find') if any(r['type'] == t for r in rows)]:
        out += [f'**{qtype}**', '', '| Tool | Language | Questions | Precision | Recall | Unjudged items |'
                + (' not_found | error | no_counterpart |' if kind == 'answered' else ''),
                '| --- | --- | ---: | ---: | ---: | ---: |' + (' ---: | ---: | ---: |' if kind == 'answered' else '')]
        for tool in TOOLS:
            for lang in LANGS:
                r = by.get((qtype, tool, lang))
                if r is None:
                    if kind == 'all_questions' and lang == 'all':
                        out.append(f'| {NAMES[tool]} | {LANG_NAMES[lang]} | no_counterpart | | | |')
                    continue
                line = (f"| {NAMES[tool]} | {LANG_NAMES[lang]} | {r['questions']} | {fmt(r['precision'])} | "
                        f"{fmt(r['recall'])} | {r['unjudged']} |")
                if kind == 'answered':
                    o = r['outcomes']
                    line += f" {o['not_found']} | {o['error']} | {o['no_counterpart']} |"
                out.append(line)
        out.append('')
    out.append('')
    return out


def repetition(a, b):
    """Answers whose outcome or item set differs between two repetitions, by tool."""
    diff = []
    for key in sorted(a):
        x, y = a[key], b.get(key)
        if y is None or x['outcome'] != y['outcome'] or {tuple(i) for i in x['items']} != {tuple(i) for i in y['items']}:
            diff.append(key)
    return diff


def variance(summary_a, summary_b):
    """Markdown rows for every figure that differs between two repetitions: tool, score, type, language, both values."""
    out = []
    for kind, label in (('answered', 'answered'), ('all_questions', 'every question')):
        b = {(r['tool'], r['type'], r['language']): r for r in summary_b[kind]}
        for r in summary_a[kind]:
            other = b.get((r['tool'], r['type'], r['language']))
            for measure in ('precision', 'recall'):
                if other and other[measure] != r[measure]:
                    out.append(f"| {NAMES[r['tool']]} | {r['type']} | {LANG_NAMES[r['language']]} | {label} | {measure} | "
                               f"{fmt(r[measure])} | {fmt(other[measure])} |")
    return out


def depth_finding(ans):
    """KnowCode's impact answers by the incomplete reasons their first page states."""
    c = Counter()
    for (tool, _), a in ans.items():
        if tool != 'knowcode' or a['type'] != 'impact' or a['outcome'] != 'judged':
            continue
        reasons = sorted(set(re.findall(r'^incomplete \((\w+)\)', a['raw'][0], re.M))) if a['raw'] else []
        c[','.join(reasons) or 'complete'] += 1
    return c


def misses_tsv(path, summary_misses, ans):
    with open(path, 'w') as f:
        f.write('tool\tquestion\twhat\tfile\tline\tdetail\n')
        for m in summary_misses:
            for x in m['missed']:
                f.write(f"{m['tool']}\t{m['id']}\tmissed\t{x[0]}\t{x[1]}\t{x[2] if len(x) > 2 else ''}\n")
            for x in m['wrong']:
                f.write(f"{m['tool']}\t{m['id']}\twrong\t{x[1]}\t{x[2]}\t{x[0]}{'' if len(x) < 4 else '-' + str(x[3])}\n")
        for (tool, qid), a in sorted(ans.items()):
            if a['outcome'] in ('not_found', 'error'):
                f.write(f"{tool}\t{qid}\t{a['outcome']}\t\t\t{reason(a)}\n")


def reason(a):
    """One line on why a question went unanswered: the tool's own message, or what the lookup found."""
    last = (a['raw'][-1] if a['raw'] else '') or ''
    if a['tool'] == 'knowcode' and a['outcome'] == 'not_found' and last.startswith(('{', 'repo ')):
        return 'the answer was for a declaration of another name'
    if last.lstrip().startswith(('{', '[')):
        return 'the lookup found no declaration of that name holding the line' if a['outcome'] == 'not_found' else 'error'
    return ' '.join(last.split())[:200]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out', required=True)
    ap.add_argument('--run', action='append', required=True)
    ap.add_argument('--note', action='append', default=[])
    args = ap.parse_args(argv)
    out = Path(args.out)
    (out/'results').mkdir(parents=True, exist_ok=True)
    notes = dict(n.split('=', 1) for n in args.note)
    lines = []
    firsts = {}
    for spec in args.run:
        name, dirs = spec.split('=', 1)
        a_dir, b_dir = dirs.split(',')
        summary = json.loads((Path(a_dir)/'summary.json').read_text())
        summary_b = json.loads((Path(b_dir)/'summary.json').read_text())
        misses = [json.loads(l) for l in (Path(a_dir)/'summary.misses.jsonl').read_text().splitlines()]
        ans_a, ans_b = answers(a_dir), answers(b_dir)
        firsts[name] = summary
        diff = repetition(ans_a, ans_b)
        same_scores = summary == summary_b
        misses_tsv(out/'results'/f'{name}.misses.tsv', misses, ans_a)
        lines += [f'## Run: {name}', '']
        if name in notes:
            lines += [notes[name], '']
        lines += [f"Repetitions: two complete runs in fresh containers. Scores identical: {'yes' if same_scores else 'no'}. "
                  f"Answers that differ between them (outcome or set of items): {len(diff)} of {len(ans_a)}.", '']
        if diff:
            lines += ['| Tool | Answers that differ |', '| --- | ---: |']
            for tool, n in sorted(Counter(k[0] for k in diff).items()):
                lines.append(f'| {NAMES[tool]} | {n} |')
            lines.append('')
        rows = variance(summary, summary_b)
        if rows:
            lines += ['### Figures that differ between the two repetitions', '',
                      '| Tool | Question | Language | Score | Measure | First repetition | Second repetition |',
                      '| --- | --- | --- | --- | --- | ---: | ---: |', *rows, '',
                      'Every other figure is the same in both repetitions. The tables below give the first repetition.', '']
        lines += ['### Scored on the questions each tool answered', '']
        lines += table(summary['answered'], 'answered')
        lines += ['### Scored on every question (`not_found` and `error` count as every label missed)', '']
        lines += table(summary['all_questions'], 'all_questions')
        depth = depth_finding(ans_a)
        lines += ['### KnowCode impact answers by what their first page says is incomplete', '',
                  '| Incomplete reasons | Answers |', '| --- | ---: |']
        for k, v in sorted(depth.items(), key=lambda kv: -kv[1]):
            lines.append(f'| {k} | {v} |')
        lines += ['', f'Every miss of every tool: [results/{name}.misses.tsv](results/{name}.misses.tsv).', '']
    (out/'results.md.body').write_text('\n'.join(lines) + '\n')


if __name__ == '__main__':
    main()
