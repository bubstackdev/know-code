# Rules against the ast-grep command line

0.8's gate item "Rules" ([plan](../../docs/plans/0.8.md#gate)): for every rule in this set, KnowCode returns the
same matches as the `ast-grep` command line run directly on the same files, at the release that matches the
embedded ast-grep-core (0.50.0). A match is (rule, file, start byte, end byte); the sets must be equal. Severities
and messages (with their metavariables and transforms filled in) are compared too.

## The set

- [rules](rules): 59 rules: eight files, one per language KnowCode's rules cover (CSharp, TypeScript, Tsx, Python,
  Rust, Swift, Java, Go), and [yaml-edges.yml](rules/yaml-edges.yml), whose five rules are written in the YAML forms where two
  parsers could differ: anchors and aliases, folded and literal scalars, quoted keys, `!!str` tags, a numeric-looking
  id, flow collections, comments and document markers. Between them they use every part of ast-grep's rule YAML that KnowCode accepts: pattern strings and
  pattern objects (`context`, `selector`), `kind`, `regex`, `nthChild`; `inside`, `has`, `precedes` and `follows`,
  with `stopBy` left at its default (`neighbor`) and set to `end`, and `field`; `all`, `any` and `not`; `matches`
  with `utils`; `constraints`; `transform` in a message; `files` and `ignores`; severities; and several rules to a
  file, with the language written as its name and as its aliases.
- [src](src): made-up source files, two or three per language, written to give each rule matches and near misses,
  including `ast-grep-ignore` comments that suppress one rule and every rule on the next line.
- [expected.jsonl](expected.jsonl): ast-grep's 105 matches, one JSON line each, written by
  `scripts/eval/rules_gate.py reference`. ast-grep labels; KnowCode labels nothing.

- [refused](refused): six rule files the ast-grep command line refuses (a merge key, which serde_yaml 0.9 doesn't
  merge; a custom tag; a number where a pattern goes; a null `not`; a duplicate field; an alias to another
  document's anchor).

Every rule matches at least once, so none passes by matching nothing on either side.

## The reference

`rules_gate.py reference` runs the release binary of ast-grep 0.50.0 (`app-aarch64-unknown-linux-gnu.zip` from
ast-grep/ast-grep's GitHub release; its SHA-256 is pinned in the script) in a throwaway `ubuntu:26.04` container with
no network, as `ast-grep scan --rule <file> --json=stream .` from [src](src), once per rule file.

The grammars ast-grep 0.50.0 was built with are the ones KnowCode embeds (tree-sitter-c-sharp 0.23.5,
tree-sitter-typescript 0.23.2, tree-sitter-python 0.25.0, tree-sitter-rust 0.24.2, tree-sitter-java 0.23.5,
tree-sitter-go 0.25.0), except Swift: ast-grep's is tree-sitter-swift 0.7.3 and KnowCode's 0.7.4.

A Go call at the top level of a pattern (`fmt.Println($$$)`) doesn't parse as a call on its own; the Go rules give it
as a pattern object with a function around it (`context`) and `selector: call_expression`, as ast-grep's own docs
advise. Both sides read it the same way.

## The check

The gate has two halves. `crates/kc-index/tests/rules_fixture.rs` loads every rule file, matches every source file,
and compares with [expected.jsonl](expected.jsonl) (105 matches); it runs with the workspace's tests, and checks that every file in
[refused](refused) is refused. `rules_gate.py verify
--sg-dir DIR` runs ast-grep in the container again and compares its matches with [expected.jsonl](expected.jsonl),
so the committed reference is ast-grep's own. To write the reference again:

```sh
python3 scripts/eval/rules_gate.py reference --sg-dir DIR --tree eval/rules-v1/src \
    --rules eval/rules-v1/rules/*.yml --out eval/rules-v1/expected.jsonl
```

where `DIR` holds the unpacked release binary.

## Not in the set

- `fix` and `rewriters`' use in fixes: KnowCode refuses a rule with `fix`, since its rules only report.
- JavaScript: KnowCode parses `.js` with the TSX grammar, ast-grep with its JavaScript grammar, so a JavaScript
  rule is refused rather than matched with other node kinds.
- Kotlin: KnowCode parses Kotlin with tree-sitter-kotlin-ng, ast-grep with its own Kotlin grammar, so a Kotlin rule
  is refused for the same reason.
- Project configuration (`sgconfig.yml`: `ruleDirs`, `utilDirs`, custom languages, language globs and
  injections): KnowCode reads rule files named on its command line or in its config.
