"""Records of the neutral benchmark: questions and their labels, one JSON object per line.

A question never says how its label was made, so a tool's operator can be handed `questions.jsonl` alone. Labels
live in a separate file and are joined on `id`.

question:
    id            "A-locate-007": source letter, type, sequence
    stratum       what kind of question (see STRATA)
    source        "history" | "ticket" | "structural"
    type          "locate" | "co-change" | "impact" | "callers" | "implementations" | "table-access" | "bus" |
                  "http-callers" | "ui-flow" | "role-users" | "deploy-chain" | "tf-deps" | "ownership"
    question      the natural-language question, as an operator would be given it
    repos         repo names the question is about (a tool may look beyond them)
    commit        {repo: sha} the corpus is pinned at for this question
    index_state   {repo: sha} the newest commit a tool's index may contain; for history questions the parent of the
                  change, so the answer can't be read back from the change itself
    answer_kind   "files" | "file_lines" | "symbols" | "people"
    meta          free-form; never the answer

label:
    id            the question's id
    items         [{"item": "repo/path[:line]" or a symbol, endpoint or person, "evidence": "repo/path:line ...",
                    "note": optional, "optional": true if a tool may omit it without penalty,
                    "via": "dispatch" if reached only through virtual or interface dispatch}]
    tests         items that are test code, kept apart from `items` (neither hit nor false positive)
    negatives     same-named but different targets; returning one is always a false positive
    completeness  "exhaustive" (precision and recall both count) | "lower_bound" (recall only) |
                  "uncertain" (recall only, and reported apart)
    match         "file" | "file_line" | "line_within:N" | "symbol" | "ci"
    dynamic_name  true when the subject's name is built at runtime (reported with and without)

    Required versus optional, so graders apply one rule: an item is required when the subject reaches it directly
    or through its own collaborators (for a UI component: its injected services, its state-management effects, and
    dialogs it opens); optional when it is reached only through something that would run anyway (persistent child
    components, app-wide singletons) or only indirectly (a provider behind a local, a wrapper's caller).
    outside_corpus  how many true answers were left out because they live in repos that are not cloned
    note          how the label was made and what could be missing
    minutes       rough labelling time
"""

SOURCES = {"history", "ticket", "structural"}
COMPLETENESS = {"exhaustive", "lower_bound", "uncertain"}
ANSWER_KINDS = {"files", "file_lines", "symbols", "people"}
STRATA = [
    # history and tickets
    "history-locate", "history-co-change", "history-impact", "ticket-locate",
    # structural
    "cs-callers", "di-implementations", "sql-table-access", "bus-pub-sub", "http-cross-repo", "angular-ui-flow",
    "cfml-callers", "ansible-role-users", "deploy-chain", "terraform-deps",
    # public source-only controls and explicitly required indirect callers
    "definition-lookup", "two-hop-impact",
]

QUESTION_FIELDS = {
    "id": str, "stratum": str, "source": str, "type": str, "question": str, "repos": list, "commit": dict,
    "index_state": dict, "answer_kind": str,
}
LABEL_FIELDS = {"id": str, "items": list, "completeness": str, "match": str}


def _check_fields(rec, fields, what):
    errs = []
    for k, t in fields.items():
        if k not in rec:
            errs.append(f"{what} {rec.get('id', '?')}: missing {k}")
        elif not isinstance(rec[k], t):
            errs.append(f"{what} {rec.get('id', '?')}: {k} should be {t.__name__}")
    return errs


def validate_question(q):
    """Problems with one question record, as a list of messages (empty when it is well formed)."""
    errs = _check_fields(q, QUESTION_FIELDS, "question")
    if errs:
        return errs
    if q["source"] not in SOURCES:
        errs.append(f"question {q['id']}: unknown source {q['source']}")
    if q["stratum"] not in STRATA:
        errs.append(f"question {q['id']}: unknown stratum {q['stratum']}")
    if q["answer_kind"] not in ANSWER_KINDS:
        errs.append(f"question {q['id']}: unknown answer_kind {q['answer_kind']}")
    if not q["repos"]:
        errs.append(f"question {q['id']}: no repos")
    for r in q["repos"]:
        if r not in q["commit"] or r not in q["index_state"]:
            errs.append(f"question {q['id']}: repo {r} has no pinned commit or index state")
    return errs


def validate_label(lab, question=None):
    """Problems with one label record; with its question, also checks the two agree."""
    errs = _check_fields(lab, LABEL_FIELDS, "label")
    if errs:
        return errs
    if lab["completeness"] not in COMPLETENESS:
        errs.append(f"label {lab['id']}: unknown completeness {lab['completeness']}")
    m = lab["match"]
    if m not in {"file", "file_line", "symbol", "ci"} and not (m.startswith("line_within:") and m[12:].isdigit()):
        errs.append(f"label {lab['id']}: unknown match {m}")
    for it in lab["items"] + lab.get("tests", []) + lab.get("negatives", []):
        if not isinstance(it, dict) or not it.get("item") or not it.get("evidence"):
            errs.append(f"label {lab['id']}: every item needs `item` and `evidence`")
            break
    if not lab["items"] and lab["completeness"] != "exhaustive":
        errs.append(f"label {lab['id']}: an empty answer must be exhaustive (a true 'nothing')")
    if question is not None and question["id"] != lab["id"]:
        errs.append(f"label {lab['id']}: joined to question {question['id']}")
    return errs


def validate_bank(questions, labels):
    """Problems across a whole bank: every question has exactly one label, ids are unique."""
    errs = []
    qids = [q.get("id") for q in questions]
    if len(set(qids)) != len(qids):
        errs.append("duplicate question ids")
    by = {}
    for lab in labels:
        if lab.get("id") in by:
            errs.append(f"label {lab.get('id')}: duplicate")
        by[lab.get("id")] = lab
    for q in questions:
        errs += validate_question(q)
        if q.get("id") not in by:
            errs.append(f"question {q.get('id')}: no label")
        else:
            errs += validate_label(by[q["id"]], q)
    for lid in set(by) - set(qids):
        errs.append(f"label {lid}: no question")
    return errs
