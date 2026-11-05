# Identity against a compiler

References sampled from pinned public repositories, each labelled with the declaration the language's own toolchain
binds it to: the Swift index store, Roslyn for C#, the TypeScript compiler for TypeScript. Neither KnowCode nor a model
labels anything. The set is 0.6's gate item "Identity, against a compiler" ([plan](../../docs/plans/0.6.md#gate)):

- **Precision of `exact` links** is at least 0.99, and every wrong one is listed.
- **No link to an indexed declaration** where the toolchain resolves outside the repository (the standard library,
  the SDK, or a package).
- **Recall** is reported per language.

This file states the labelling rule. It was written and committed before KnowCode's answers on the set were read, and
it doesn't change to fit them. A change to the rule is a new set (identity-v2).

## Repositories

[repos.txt](../../scripts/eval/identity/repos.txt) pins them, at the commits the replay gate (public-v2's manifest)
and the workspace gate already use:

| Language | Repositories |
| --- | --- |
| Swift | swift-argument-parser, swift-collections, swift-nio |
| C# | serilog, autofac, jellyfin-server |
| TypeScript | jellyfin-web, hono |

Each repository is its own workspace: KnowCode indexes it alone, and anything outside it is outside, including a
dependency that is itself a public repository.

## Labelling

Every step that restores or builds a repository runs in a throwaway Linux container
([run.sh](../../scripts/eval/identity/run.sh)): the pinned clone is copied in, the network is up only for the restore,
and only the label files come out. The labeller for each language enumerates every reference the toolchain binds in
the repository's own source files, and every declaration it sees there.

| Language | Toolchain | Build | Source files |
| --- | --- | --- | --- |
| Swift | the index store, read through `libIndexStore` | `swift build --build-tests --enable-index-store` | every target's, tests included |
| C# | Roslyn's semantic model over `MSBuildWorkspace` | the solution, after `dotnet restore` | every project's compile items |
| TypeScript | the compiler API's type checker | the repository's `tsconfig.json`, after `npm ci --ignore-scripts` (pnpm for hono) | the program's files |

The toolchains, pinned in run.sh by image digest:

| Language | Image | Toolchain |
| --- | --- | --- |
| Swift | `swift:6.3-noble` (`sha256:1038d54c…`) | Swift 6.3.3, its `libIndexStore` |
| C# | `ubuntu:26.04` (`sha256:19fcd1bf…`) | Ubuntu's `dotnet-sdk-10.0` (SDK 10.0.113), Roslyn 5.0.0 (Microsoft.CodeAnalysis), Microsoft.Build.Locator 1.9.1 |
| TypeScript | `node:24-bookworm` (`sha256:6449e19d…`) | Node 24.22.1; each repository's own TypeScript (5.9.3 for jellyfin-web, 6.0.3 for hono) |

serilog and autofac load with one framework per project (`TargetFrameworks=net10.0`,
[properties.txt](../../scripts/eval/identity/csharp/properties.txt)). In autofac, Autofac.Test.CodeGen (net8.0 only) loads
without its framework references, so its references to the base class library were never bound: the draw leaves out
every reference in its files (`EXCLUDED` in sample.py), so none becomes a label or a gap KnowCode is scored against.

Files under build output (`.build`, `bin`, `obj`), dependency checkouts and `node_modules` are not the repository's
source, and generated files (`*.g.cs`, `*.Designer.cs`, macro expansions) are left out.

### What a reference is

A name token in source that the toolchain binds to a declaration, and that isn't itself a declaration of that symbol.
Left out:

- **Implicit references:** the toolchain marks them implicit (a synthesised conformance, an implicit `self`), or they
  have no token of their own.
- **Constructors and initialisers:** `Foo(…)`, `new Foo(…)`, `super.init`. KnowCode treats an instantiation as a
  reference to the type, the toolchains as one to the initialiser; the two don't compare.
- **Locals, parameters and generic parameters:** declarations inside a function body or signature.
- **Operators, keywords and accessors:** `+`, `self`, `this`, `base`, `super`, `get`/`set`, subscripts.
- **Imports:** `import`, `using`, and TypeScript `import`/`export` lists.
- **Renamed references:** the token differs from the declaration's own name (an import alias, a type alias's target).
- **C:** references in the C files of a Swift package, and Swift references to declarations in its C modules.
- **Synthesised members:** a member the compiler makes for one of the repository's types (an enum's `rawValue`), which
  has no declaration in source.
- **Anything the toolchain can't bind:** an error type, or a member of `any`.

### Kinds

Each reference has one kind, the first that applies:

1. `extension`: a member declared in an extension (a Swift extension, a C# extension method). TypeScript has none.
2. `dispatch`: a member declared by a protocol or an interface, or an abstract member: the call is satisfied through a
   conformance.
3. `call`: a function or method, called or passed.
4. `member`: a property, field, event or enum case read or written.
5. `type`: a type named: a class, struct, enum, protocol, interface, type alias or delegate.

`extension` and `dispatch` are kinds of member: a type declared in an extension is a `type`. The Swift index store
holds no declaration for a standard library or SDK target, so a Swift reference to one is `call`, `member` or `type`.

### The target

The declaration the toolchain binds the reference to, taken back to its original definition: a generic
instantiation's definition, an import alias's target, a C# reduced extension method's static definition. An override
counts as its own declaration: the one the toolchain binds is the label.

- **Same declaration:** the same file (repository-relative) and the same line of the declaration's name token. A
  declaration with several parts (a C# partial method or type, TypeScript overloads and merged declarations) lists
  every part, and any one of them matches.
- **Inside or outside:** inside when the declaration is in one of the repository's own source files. Otherwise
  outside, with its origin: `stdlib` (the Swift standard library and core libraries, the .NET base class library,
  TypeScript's `lib.*.d.ts`), `sdk` (other platform modules and frameworks), or `package` (a SwiftPM dependency, a NuGet
  package, `node_modules`).

### Multiplicity

How many declarations in the repository have the target's simple name, counted over the declarations the toolchain
saw: types, functions and methods, properties and fields, events and enum cases, each once however many parts it has.
Locals, parameters, generic parameters, accessors and initialisers are not counted. Buckets: `1`, `2-4`, `5+`; for an
outside target, `0` (no indexed declaration of that name) or `1+` (an indexed declaration shares its name: the case a
name match gets wrong).

## Sampling

[sample.py](../../scripts/eval/identity/sample.py) draws the set from the full enumeration, the same way for every
language, weighted toward names declared more than once:

- **Strata:** per repository, kind × bucket. Each inside stratum takes up to 20 references for bucket `1` and up to
  40 for `2-4` and `5+`; each outside stratum up to 10 for `0` and 40 for `1+`.
- **Order:** within a stratum, references are taken in the order of the SHA-256 of `repo:file:line:col`, so the draw
  is fixed by the enumeration alone.
- **Spread:** at most 3 references per target declaration in a stratum, so a few common names don't fill it.
- **One row per reference:** a reference sampled into one stratum isn't drawn again.

`labels.jsonl` holds the set and `strata.json`, per stratum, how many references there were and how many were drawn.
The enumerations it was drawn from (about 10 MB compressed) aren't checked in: run.sh makes them again, and
`enumerations.sha256` holds the digests of the ones drawn from. Each label records its repository, file, line and 1-based UTF-8 byte column, the name, kind, bucket, the
target (inside with its locations, or outside with its origin), and the toolchain's own name for the symbol.

## Scoring

[identity_gate.py](../../scripts/eval/identity_gate.py) asks KnowCode, on the Mac, over the same pinned clones, with
its own cache, config and data directories. Per repository and name, one `refs NAME --json --cite none` answer. A
reference's row in it:

- **Its row:** the row of the same file, line and name whose column is the label's; failing that, the only row of
  that name on the line, when the name appears once on the line. No row means KnowCode has no reference there.
- **Its targets:** the row's `targets`, or, for a call of an overloaded name, the entries of the answer's numbered
  `overloads` list that its `overload` names. A KnowCode target matches the label when it is in the same file and its name line is one of the
  label's lines. The name line is the first of the target's line and the next 10 that has the name as a word outside
  comments, attributes and decorators (a declaration that starts at one of those); a leading block of attributes or
  annotations, of any length, is skipped before the 10 are counted.

Reported, per language, per repository and per stratum:

- **Precision per confidence tag:** of the rows KnowCode tags `exact` (and `unique_name`, and the others), the share
  whose one target matches the label. An outside label linked to any indexed declaration is wrong. Every wrong `exact`
  row is listed with its file:line, KnowCode's target and the label's.
- **Ambiguous rows:** the share whose candidates include the label, and the mean number of candidates.
- **Recall:** of the inside labels, the share KnowCode links to the label (`exact` or `unique_name`, one target), and
  the share where the label is among its candidates (`ambiguous` included).
- **Linked inside where the toolchain says outside:** outside labels KnowCode links (`exact` or `unique_name`) to an
  indexed declaration, listed; and, separately, those it lists as `ambiguous` candidates.

The gate passes when `exact` precision is at least 0.99 in each language and no outside label is linked.

## The set

4943 references: 1864 Swift, 1931 C#, 1148 TypeScript. Per language, kind and stratum (inside by bucket, outside
by bucket):

| Language | Kind | inside 1 | inside 2-4 | inside 5+ | outside 0 | outside 1+ | All |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Swift | extension | 60 | 120 | 120 | 10 | 40 | 350 |
| Swift | dispatch | 0 | 75 | 118 | 0 | 0 | 193 |
| Swift | call | 60 | 120 | 120 | 30 | 120 | 450 |
| Swift | member | 60 | 120 | 120 | 30 | 120 | 450 |
| Swift | type | 60 | 120 | 120 | 30 | 91 | 421 |
| C# | extension | 43 | 83 | 66 | 30 | 79 | 301 |
| C# | dispatch | 14 | 96 | 120 | 30 | 107 | 367 |
| C# | call | 60 | 120 | 120 | 30 | 120 | 450 |
| C# | member | 60 | 120 | 120 | 30 | 120 | 450 |
| C# | type | 60 | 115 | 88 | 30 | 70 | 363 |
| TypeScript | dispatch | 40 | 80 | 80 | 20 | 80 | 300 |
| TypeScript | call | 40 | 80 | 80 | 20 | 72 | 292 |
| TypeScript | member | 40 | 80 | 80 | 20 | 60 | 280 |
| TypeScript | type | 40 | 80 | 69 | 20 | 67 | 276 |

A protocol requirement is always declared alongside its witnesses, so Swift has no `dispatch` reference in bucket `1`,
and an outside Swift `dispatch` or `extension` target outside a package has no declaration to classify it by. A
stratum short of its quota held fewer references.

## Baseline: 0.5.0

The release binary (`knowcode-aarch64-apple-darwin`, checksum verified) and integration/0.6 at ef9cdd3 judge every
label the same. Descriptive figures.

| Language | Labels | Inside | `exact` rows | `exact` precision | `unique_name` precision | Recall, linked | Recall, among candidates | Outside linked |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Swift | 1864 | 1393 | 719 | 0.674 | 0.776 | 0.450 | 0.775 | 52 |
| C# | 1931 | 1285 | 976 | 0.839 | 0.844 | 0.730 | 0.873 | 37 |
| TypeScript | 1148 | 789 | 475 | 0.872 | 0.342 | 0.575 | 0.720 | 84 |

The gate fails in all three languages. The 452 wrong `exact` rows, by what went wrong:

| Cause | What went wrong | Swift | C# | TypeScript |
| --- | --- | ---: | ---: | ---: |
| `several_with_label` | Several declarations under one `exact` row, the label among them (overloads, same-named members in scope) | 161 | 132 | 28 |
| `several_without_label` | Several declarations, the label not among them (a protocol requirement called: its witnesses listed instead) | 23 | 0 | 17 |
| `linked_outside` | Linked inside where the toolchain resolves outside (stdlib names a local declaration shadows, or doesn't) | 31 | 18 | 15 |
| `one_wrong` | One declaration, the wrong one (a requirement for a concrete receiver's member, a property for a same-named type) | 19 | 7 | 1 |

Some of them:

- swift-nio: `pipeline`, `isActive` and `localAddress` on an `EmbeddedChannel` link to the `Channel` protocol's
  requirements, not to `EmbeddedChannel`'s own members.
- swift-collections: `next()` in `BigSubstring`'s iterators links to five other types' `next()` in other modules.
- RopeModule's generic code calls `RopeSummary` requirements (`maxNodeSize`, `nodeSizeBitWidth`); they link to the
  two conformances' members instead.
- jellyfin-server: `SourceType` used as a type in `BaseItem` links to `BaseItem`'s property of that name, not the enum.
- swift-argument-parser: `append` on a standard library string in the completion and help generators links to the manual
  tool's own `append`.

Outside labels linked `unique_name`: 21 Swift, 19 C#, 69 TypeScript. Most of TypeScript's are DOM types hono names
(`MouseEvent`, `KeyboardEvent`) linked to its JSX module's same-named types, and jellyfin-web's calls of its API
client package linked to a local declaration file's same-named members.

The full list, the identity work's to-do, is 452 wrong `exact` links and 109 outside labels linked `unique_name`, each
with its place, KnowCode's targets and the label. It is generated output, kept with the evaluation's evidence rather
than here; the committed scorer makes it again from the committed labels:

```sh
python3 scripts/eval/identity_gate.py run --knowcode BIN --work WORK --out summary.json
python3 scripts/eval/identity_gate.py report summary.json --wrong 100000
```

Scoring takes under a minute on a loaded development Mac (8 indexes, 2462 `refs` answers); the budget is 30 minutes.

**The scorer was corrected after its first run**, before these figures: it read only a row's `targets`, not the
numbered `overloads` an answer lists once for every row that calls one, and it took a doc comment or attribute naming
the declaration as the declaration's name line. Neither touched the labels or the rule for them.

**Corrected again on 2026-10-29**, while identity-v2's Kotlin labels were first scored: a declaration under an
annotation longer than the window (kotlinpoet's `inner class IsAnnotated`, below a 12-line `@HasDefaultsAnnotation(…)`)
had its name line taken as the annotation's first line, so 3 links KnowCode made correctly were judged wrong. The
window now starts after a declaration's leading attribute or annotation block, as the rule above always said
attributes come before the name. The labels didn't change. Judged by the scorer before and after with
one KnowCode build, identity-v1 and identity-v2 (Go, its held-out set, JVM and both JVM held-out sets: 10,251
labels) changed 9 judgements, each to agree with the toolchain: in identity-v1, 2 C# `exact` rows under a 20-line
`[InlineData]` block judged right, and 2 Swift `ambiguous` rows now counted as listing the label; in identity-v2, the 3
Kotlin rows above judged right. No judgement went the other way.

## Running it

```sh
# labels (slow; Docker; only label files come out)
scripts/eval/identity/run.sh REPO CLONE IO   # each repository; OUT is IO/out
python3 scripts/eval/identity/sample.py OUT/enumeration-*.jsonl.gz --out eval/identity-v1/labels.jsonl \
  --strata eval/identity-v1/strata.json
# scoring (the Mac; KnowCode only)
python3 scripts/eval/identity_gate.py fetch --work WORK
python3 scripts/eval/identity_gate.py run --knowcode BIN --work WORK --out summary.json
python3 scripts/eval/identity_gate.py check summary.json      # exits 1 unless the gate passes
```

`run` writes the summary, each label's judgement (`summary.judged.jsonl`), and one file per cause
(`summary.causes/<cause>.md`) listing its wrong `exact` rows. These are generated output: keep them with the
evaluation's evidence, not in the repository.

**A change to resolution** shows which causes it moves: run the base build and the change on the same labels, each
into its own summary, and put `compare`'s table in the pull request. It gives the gate's figures and each cause per
language, base → candidate, and lists every label the change makes newly wrong.

```sh
python3 scripts/eval/identity_gate.py run --knowcode BASE_BIN --work WORK --out base.json
python3 scripts/eval/identity_gate.py run --knowcode target/release/knowcode --work WORK --out change.json
python3 scripts/eval/identity_gate.py compare base.json change.json
```

**A release** runs it against the previous release and the candidate, publishes the two summaries' figures and the
`compare` table in its release gate (eval/release-gate/), and ships only when `check` passes on the candidate
([releasing.md](../../docs/releasing.md#before-tagging)).
