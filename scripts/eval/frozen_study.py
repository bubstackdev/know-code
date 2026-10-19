"""Validate a public-v1 study freeze from content, never from a readiness flag.

The freeze names immutable input files and implementation hashes. No model,
indexer, client or network operation occurs here. Independent review evidence
remains an attributable source-only attestation, not an automated truth proof.
"""
from collections import Counter
import hashlib
import importlib.util
import json
from pathlib import Path
import re
from public_transcript import load_json

ROOT=Path(__file__).resolve().parents[2]
FILES={'manifest','source_receipt','plan','questions','labels','order','config','scope','sampling',
       'candidates','provisional','subject_review','question_subjects','review','method','sampling_rules'}
PRODUCERS={
    'scripts/plan-public-eval.py','scripts/census-public-eval.py','scripts/prepare-public-eval-session.py',
    'scripts/prepare-public-eval-index.py','scripts/eval/neutral/schema.py','scripts/eval/frozen_study.py',
    'scripts/eval/session_runner.py','scripts/eval/bare_client.py','scripts/eval/macos_sandbox.py',
    'scripts/eval/owned_process.py','scripts/eval/model_relay.py','scripts/eval/public_transcript.py',
}
REVIEW_SEED='knowcode-public-v1-independent-review-2026-10-16'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return digest(json.dumps(value,sort_keys=True,separators=(',',':')).encode())


def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module


class FrozenStudy:
    def __init__(self,path):
        self.path=Path(path).resolve(strict=True)
        self.raw=self.path.read_bytes()
        self.freeze_sha256=digest(self.raw)
        specification=load_json(self.raw)
        if specification.get('format')!='knowcode-public-v1-freeze-1' or set(specification.get('files',{}))!=FILES:
            raise ValueError('complete public-v1 freeze file inventory required')
        self.captured={self.path:self.raw}
        self.inputs={}
        self.hashes={}
        for name,entry in specification['files'].items():
            if not isinstance(entry,dict) or set(entry)!={'path','sha256'} or not re.fullmatch('[0-9a-f]{64}',entry['sha256']):
                raise ValueError('invalid frozen input descriptor')
            candidate=Path(entry['path'])
            candidate=candidate if candidate.is_absolute() else self.path.parent/candidate
            if candidate.is_symlink() or not candidate.is_file():
                raise ValueError('regular nonsymlink frozen input required')
            candidate=candidate.resolve(strict=True)
            data=candidate.read_bytes()
            if digest(data)!=entry['sha256']:
                raise ValueError('frozen input hash differs: '+name)
            self.inputs[name]=candidate;self.hashes[name]=entry['sha256'];self.captured[candidate]=data
        producer_hashes=specification.get('producers')
        if not isinstance(producer_hashes,dict) or not PRODUCERS.issubset(producer_hashes):
            raise ValueError('complete runner implementation hash inventory required')
        for name,expected in producer_hashes.items():
            relative=Path(name)
            if relative.is_absolute() or '..' in relative.parts or not name.startswith('scripts/'):
                raise ValueError('invalid frozen implementation path')
            candidate=ROOT/relative
            if candidate.is_symlink() or not candidate.is_file() or digest(candidate.read_bytes())!=expected:
                raise ValueError('runner implementation differs: '+name)
            self.captured[candidate.resolve()]=candidate.read_bytes()
        self.client_sha256=specification.get('client_sha256')
        if not isinstance(self.client_sha256,str) or not re.fullmatch('[0-9a-f]{64}',self.client_sha256):
            raise ValueError('frozen client executable hash required')
        self.schema=load('frozen_bank_schema',ROOT/'scripts/eval/neutral/schema.py')
        self.planner=load('frozen_public_plan',ROOT/'scripts/plan-public-eval.py')
        self.manifest=self.read('manifest')
        self.plan=self.read('plan')
        self.config=self.read('config')
        self.questions=self.jsonl('questions')
        self.labels=self.jsonl('labels')
        self.order=self.jsonl('order')
        self.planner.check_questions(self.questions,self.manifest,self.schema)
        self.planner.check_config(self.config)
        errors=self.schema.validate_bank(self.questions,self.labels)
        if errors:
            raise ValueError('complete valid label bank required: '+'; '.join(errors))
        if len(self.questions)!=24 or Counter(q['stratum'] for q in self.questions)!={k:6 for k in self.planner.TASKS}:
            raise ValueError('all24 prespecified public questions required')
        encoded=''.join(json.dumps(q,sort_keys=True)+'\n' for q in sorted(self.questions,key=lambda q:q['id'])).encode()
        bindings={'manifest_sha256':self.hashes['manifest'],'source_receipt_sha256':self.hashes['source_receipt'],
                  'questions_input_sha256':self.hashes['questions'],'operator_questions_sha256':digest(encoded),
                  'order_sha256':self.hashes['order'],'config_sha256':self.hashes['config']}
        if any(self.plan.get(key)!=expected for key,expected in bindings.items()) or self.plan.get('config')!=self.config:
            raise ValueError('plan differs from frozen inputs/configuration')
        if self.order!=self.planner.schedule(self.questions,self.plan['seed']) or len(self.order)!=48:
            raise ValueError('paired schedule differs from the frozen order rule')
        self.receipt=self.read('source_receipt')
        if self.receipt.get('manifest_sha256')!=self.hashes['manifest']:
            raise ValueError('source receipt belongs to a different cohort')
        expected={r['name']:dict(r,family=f['name']) for f in self.manifest['families'] for r in f['repos']}
        if len(self.receipt['repos'])!=len(expected) or {r['name'] for r in self.receipt['repos']}!=set(expected):
            raise ValueError('source receipt repository set differs')
        for repo in self.receipt['repos']:
            if any(repo.get(k)!=v for k,v in expected[repo['name']].items()):
                raise ValueError('source pin, scope or license metadata differs')
        sampling=self.read('sampling')
        if (sampling.get('scope_sha256')!=self.hashes['scope'] or sampling.get('candidates_sha256')!=self.hashes['candidates']
                or sampling.get('provisional_sha256')!=self.hashes['provisional']):
            raise ValueError('sampling inputs are not hash-bound')
        subjects=self.read('question_subjects');accepted=self.read('subject_review')
        if accepted.get('sampling_sha256')!=self.hashes['sampling'] or len(subjects)!=24:
            raise ValueError('accepted sampling review required')
        links={row['id']:row['candidate_id'] for row in subjects}
        if set(links)!={q['id'] for q in self.questions} or len(set(links.values()))!=24:
            raise ValueError('unique question-to-source-subject linkage required')
        accepted_rows=[row for row in accepted['accepted'] if row.get('decision')=='accept']
        valid={row['candidate_id'] for row in accepted_rows}
        candidates=self.jsonl('candidates')
        population={row['id'] for row in candidates}
        if len(valid)!=len(accepted_rows) or len(population)!=len(candidates):
            raise ValueError('duplicate accepted subject or candidate')
        if set(links.values())!=valid or not valid.issubset(population):
            raise ValueError('questions differ from accepted source subjects')
        self.check_review()
        self.unchanged()

    def read(self,name):
        return load_json(self.captured[self.inputs[name]])

    def jsonl(self,name):
        return [load_json(line) for line in self.captured[self.inputs[name]].decode().splitlines() if line.strip()]

    def check_review(self):
        review=self.read('review')
        if (review.get('status')!='complete' or not isinstance(review.get('reviewer'),str) or not review['reviewer'].strip()
                or review.get('source_only') is not True or review.get('primary_labels_read') is not False
                or review.get('compared_tool_outputs_read') is not False or review.get('questions_sha256')!=self.hashes['questions']):
            raise ValueError('attributable independent blind source review required')
        subset=[]
        for stratum in sorted(self.planner.TASKS):
            ranked=sorted((q['id'] for q in self.questions if q['stratum']==stratum),
                          key=lambda qid:(digest((REVIEW_SEED+'\0'+qid).encode()),qid))
            subset.extend(ranked[:2])
        independent={r['id']:r for r in review.get('labels',[])}
        decisions={r['id']:r for r in review.get('adjudications',[])}
        if (len(independent)!=len(review.get('labels',[])) or len(decisions)!=len(review.get('adjudications',[]))
                or not set(independent).issubset({q['id'] for q in self.questions})
                or not set(decisions).issubset(independent)):
            raise ValueError('duplicate or unrelated independent review records')
        final={r['id']:r for r in self.labels};questions={r['id']:r for r in self.questions}
        if not set(subset).issubset(independent) or not set(subset).issubset(decisions):
            raise ValueError('all eight seeded review questions require labels and adjudications')
        for qid in subset:
            item=decisions[qid]
            if (self.schema.validate_label(independent[qid],questions[qid])
                    or item.get('independent_label_sha256')!=canonical(independent[qid])
                    or item.get('final_label_sha256')!=canonical(final[qid])
                    or not isinstance(item.get('resolution'),str) or not item['resolution'].strip()):
                raise ValueError('independent label or adjudication binding differs')

    def unchanged(self):
        if any(path.read_bytes()!=data for path,data in self.captured.items()):
            raise ValueError('frozen study input or implementation changed')

    def binaries(self,client,knowcode):
        for path,expected in [(Path(client),self.client_sha256),(Path(knowcode),self.config['knowcode_sha256'])]:
            if path.is_symlink() or not path.is_file() or digest(path.read_bytes())!=expected:
                raise ValueError('executable differs from frozen configuration')
