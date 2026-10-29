# ast-grep-config, vendored

ast-grep's rule configuration crate: rule YAML (`rule`, `constraints`, `utils`, `transform`, `files`/`ignores`,
severities, messages), the matchers it builds over ast-grep-core, and `CombinedScan` with `ast-grep-ignore`
suppression. KnowCode's `rules` runs on it so that a rule matches what the ast-grep command line matches.

- **Upstream:** [ast-grep/ast-grep](https://github.com/ast-grep/ast-grep), `crates/config`, release `0.50.0`, commit
  `4118186ecca8f4935ae8d85ee6d43c3211f3b763` (the source of the `ast-grep-config` 0.50.0 crate on crates.io). It
  matches the embedded `ast-grep-core` 0.50.0.
- **Licence:** MIT, [LICENSE-MIT](LICENSE-MIT) (upstream's), unchanged. KnowCode's changes are under the same
  licence.

## Why it is vendored

Upstream reads YAML with serde_yaml, which depends on unsafe-libyaml. That crate is unmaintained (RUSTSEC-2024-0449)
and is a machine translation of libyaml's C into unsafe Rust. Rule files can come with the repo under review, so a
rule file is hostile input. KnowCode already reads YAML with yaml-rust2 (safe Rust), behind its limits on size and
on the nodes aliases stand for. Here the rule YAML is read the same way, and nothing else changes.

## What changed (the YAML layer only)

- `src/yaml.rs` (new): parses the text with yaml-rust2 into nodes and deserializes them the way serde_yaml 0.9
  deserializes events:
  - plain scalars resolve as serde_yaml resolves them (null, booleans, integers, floats, else strings);
  - `!!` core tags are honoured, and another tag names an enum variant;
  - a single-key mapping is an enum variant (serde_yaml's `singleton_map_recursive`);
  - `<<` is an ordinary key;
  - an alias names an anchor of its own document only;
  - a text with no document reads as an empty struct.
- `src/lib.rs`: `from_str` and `from_yaml_string` use `yaml` instead of serde_yaml's deserializer, so they take
  `DeserializeOwned` rather than a borrowed `Deserialize<'de>`.
- `src/rule_config.rs`: `RuleConfig::deserialize` takes a `yaml::Node`. `Metadata` holds `serde_json::Value`s, and
  the error type is `yaml::Error`.
- `src/rule_core.rs`: the error type is `yaml::Error`.
- `src/transform/parse.rs`: a string transform's arguments are read with `yaml::from_str`.
- Tests: `fixer.rs`, `rule/stop_by.rs` and `transform/trans.rs` read YAML through `yaml` (`stop_by`'s round trip
  serializes with serde_json), and `lib.rs`'s test module imports `serde::Deserialize` itself.
- `Cargo.toml`: serde_yaml is replaced by yaml-rust2 and serde_json, and the crate isn't published.

One change outside the YAML layer, in tests only: in `rule/relational_rule.rs`, five test inputs that assign a function
expression assign an arrow function to `probe` instead, a wording KnowCode's repository checks require. Each is a source the rule must
not match, and still isn't.

Upstream's own tests (240) pass. eval/rules-v1 holds the rest of the check: its rules' matches equal the ast-grep
command line's (89 of 89), including rules written with anchors, aliases, folded and literal scalars, quoted keys,
tags and flow collections. Every rule file the command line refuses (a merge key, a custom tag, a number where a
pattern goes, a null `not`, a duplicate field, an alias to another document's anchor) is refused here too.

## Updating it

Replace `src/` with the new release's `crates/config/src`, apply the changes above again (only `yaml.rs` is new),
match the new release's `ast-grep-core`, then run this crate's tests and `scripts/eval/rules_gate.py verify` with that
release's ast-grep binary.
