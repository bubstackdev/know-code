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
- **Anything the toolchain can't bind:** an error type, or a member of `any`.

### Kinds

Each reference has one kind, the first that applies:

1. `extension`: a member declared in an extension (a Swift extension, a C# extension method). TypeScript has none.
2. `dispatch`: a member declared by a protocol or an interface, or an abstract member: the call is satisfied through a
   conformance.
3. `call`: a function or method called.
4. `member`: a property, field, event or enum case read or written.
5. `type`: a type named: a class, struct, enum, protocol, interface, type alias or delegate.

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

`labels.jsonl` holds the set; `enumeration-*.jsonl.gz` the full enumeration it was drawn from, so the draw can be
checked. Each label records its repository, file, line and 1-based UTF-8 byte column, the name, kind, bucket, the
target (inside with its locations, or outside with its origin), and the toolchain's own name for the symbol.

## Scoring

[identity_gate.py](../../scripts/eval/identity_gate.py) asks KnowCode, on the Mac, over the same pinned clones, with
its own cache, config and data directories. Per repository and name, one `refs NAME --json --cite none` answer. A
reference's row in it:

- **Its row:** the row of the same file, line and name whose column is the label's; failing that, the only row of
  that name on the line, when the name appears once on the line. No row means KnowCode has no reference there.
- **Its targets:** a KnowCode target matches the label when it is in the same file and its name line is one of the
  label's lines. The name line is the target's line when the name is on it, else the first of the next 10 lines that
  has the name as a word (a declaration that starts at an attribute or a doc comment).

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

## Running it

```sh
# labels (slow; Docker; only label files come out)
scripts/eval/identity/run.sh REPO OUT
python3 scripts/eval/identity/sample.py OUT/*.jsonl.gz --out eval/identity-v1/labels.jsonl
# scoring (the Mac; KnowCode only)
python3 scripts/eval/identity_gate.py fetch --work WORK
python3 scripts/eval/identity_gate.py run --knowcode BIN --work WORK --out summary.json
```
