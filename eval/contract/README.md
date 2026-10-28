# Contract gate

0.6's gate item "Contract" ([plan](../../docs/plans/0.6.md#gate)): what KnowCode says it offers, checked against what
its server does. [contract_gate.py](../../scripts/eval/contract_gate.py) makes no model call, clones nothing and runs
no third-party code: its repositories are made-up fixtures it writes into its work directory, and the network is
watched from the machine itself with loopback-only means.

## Checks

**Capabilities** (`contract_gate.py capabilities --knowcode BIN --work WORK`): the server (`serve --tools
all,kc_capabilities`, then `--tools one`) over a fixture repository with two authors' history. For every op that
`kc {op: capabilities}` describes:

- **Its input:** each parameter the advertised tool lists is in the entry, with the same type, choices and bounds (the
  advertised schema is kept shorter); each parameter the entry lists is taken: a value of its type isn't refused for
  it.
- **An answer:** a call with sample arguments answers, and a JSON answer has the keys `output.keys` promises.
- **Ranges and choices:** past a range's maximum, under its minimum, negative, and past its type's maximum, a value
  is clamped or refused as `above_max`, `below_min`, `negative` and `above_type_max` say (an older build's `beyond`);
  a value of the wrong type is refused with `wrong_type`'s code; a word outside a closed choice is refused with its `code`.
- **The single tool:** `kc {op, q}`, and `kc {op, qs}` where the op takes several, reach the op.
- **Writes:** the read ops leave the notes as they were; `note_add`, the one write, changes them.
- **Codes:** every refusal carries a code from `codes`, at `errors.at`, with `errors.keys`.

**Restricted mode** (`contract_gate.py restricted --knowcode BIN --work WORK`): `serve --restricted` bound to one
fixture repository, with `--ops find,refs`, explicit cache and config directories, an empty home and temporary
directory, and every proxy variable pointing at a loopback listener; once with the tools named, once with the single
`kc` tool:

- **Scope:** the `initialize` reply states it (the bound repository; not the other one), and only the allowed tools
  (and `kc_capabilities`) are listed.
- **Inside it:** `find`, `refs` and `capabilities` about the bound repository answer, and no answer names the other.
- **Outside it:** a call naming the other repository, by name or path, a reference given as a path in it, and an op
  not allowed (`context`, `search`, `note_add`) are refused with `not_allowed`.
- **Writes:** no file outside the cache directory is created, changed or removed: both repositories with their `.git`,
  the home, temporary and config directories are hashed before and after.
- **Network:** the server and its children are polled with `lsof -i` throughout, and must hold no network socket; the
  loopback proxy listener must see no connection.

**Codes across the replay** (`contract_gate.py codes BASE_CALLS CANDIDATE_CALLS`, the `calls.jsonl` the replay gate
writes beside each build's summary): every refusal is coded, and a call refused by both builds keeps its code. With
`--same-build` (one build replayed twice), every call is refused or answered as before and every answer's
`incomplete` and `more` entries keep their codes.

Each command exits 1 on a failure and lists every one.

## On integration/0.6 at f112ff2

- **Capabilities:** 27 ops; one failure. `search` describes `context` as clamped beyond its range (0 to 10), but
  `context: -1` is refused (`invalid_argument`: the parameter is unsigned). Either the entry says a value below 0 is
  refused, or the server clamps it.
- **Restricted mode:** passes: the scope stated, the allowlist kept, no file written outside the cache, no network
  socket and no connection to the proxy.
- **Codes:** passes. f112ff2 replayed twice: 686 calls, every refusal and entry code the same. ad88228 against f112ff2:
  686 calls, no refusal's code changed and every refusal coded; three answers' entries changed with their answers
  (an impact and a trace walk now stopping at the evidence policy, a context bundle no longer cut).

The capabilities and restricted checks take under a minute; the codes check reads two replay runs (about two minutes
each on a loaded development Mac).
