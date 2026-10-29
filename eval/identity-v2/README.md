# Identity against a compiler: Go, Java and Kotlin

References sampled from pinned public repositories, each labelled with the declaration the language's own toolchain
binds it to. Neither KnowCode nor a model labels anything. The set is 0.7's gate item "Identity, Go, Java and Kotlin"
([plan](../../docs/plans/0.7.md#gate)), per language:

- **At least 1,000 labels**, over at least two repositories.
- **Precision of `exact` links** is at least 0.99, and every wrong one is listed.
- **No link to an indexed declaration** where the toolchain resolves outside the repository.
- **Recall**, linked and among candidates, is reported.

identity-v2 keeps [identity-v1](../identity-v1/README.md)'s rule: what a reference is, its kind, its target, the
multiplicity buckets, the draw and the scoring. This file states only what v2 adds or reads differently, per language.
Each language's section, with its labels, is committed before KnowCode's answers on that language are read, and it
doesn't change to fit them. A change to the rule is a new set.

Each language keeps its labels in its own directory: `labels.jsonl`, `strata.json` (per stratum, how many references
there were and how many were drawn), `exclusions.json` (per reason, how many references the toolchain binds the rule
leaves out) and `enumerations.sha256` (the digests of the enumerations drawn from, which aren't checked in).

## Java and Kotlin

Java and Kotlin are one family, as KnowCode reads them: a name in one may bind to a declaration in the other. Each
label records the language of the file it is in (`lang`), and a repository that mixes the two is drawn once per
language, so each language's strata fill on their own. The labels are in [jvm/](jvm/labels.jsonl).

### Repositories

[repos.txt](../../scripts/eval/identity/repos.txt) pins them at their heads on 2026-10-29:

| Repository | Labels from | Build |
| --- | --- | --- |
| [google/gson](https://github.com/google/gson) | Java | Maven |
| [apache/commons-lang](https://github.com/apache/commons-lang) | Java | Maven |
| [spring-projects/spring-petclinic](https://github.com/spring-projects/spring-petclinic) | Java | Maven |
| [square/moshi](https://github.com/square/moshi) | Java and Kotlin | Gradle |
| [square/kotlinpoet](https://github.com/square/kotlinpoet) | Kotlin | Gradle (Kotlin Multiplatform, its JVM targets) |
| [spring-petclinic/spring-petclinic-kotlin](https://github.com/spring-petclinic/spring-petclinic-kotlin) | Kotlin | Gradle |

square/javapoet was a candidate and was replaced by commons-lang before any label was drawn: its build compiles with
an older javac that can't read the JDK 17 class files.

Each repository is its own workspace, as in v1. A dependency that is itself a public repository is outside.

### Labelling

[run.sh](../../scripts/eval/identity/run.sh) copies the pinned clone into a throwaway Linux container, builds it once
with the network up so that every dependency, plugin and compiler is fetched, throws that build's output away,
disconnects from the network, and builds again offline (Maven `-o`, Gradle `--offline`). The offline build is the one
labelled. [jvm/label.py](../../scripts/eval/identity/jvm/label.py) reads its index through
[scip.py](../../scripts/eval/identity/scip.py), a reader of the SCIP format written with Python's standard library.

| Toolchain | Image | Versions |
| --- | --- | --- |
| scip-java, with its javac plugin and its Kotlin compiler plugin (scip-kotlinc) | `ghcr.io/scip-code/scip-java:0.13.2` (`sha256:459c2fa0…`) | scip-java 0.13.2, Maven 3.9.1, each repository's own Gradle wrapper, and the JDKs below |

The image doesn't hold the JDKs a build runs with: its `scip-java` command fetches them with coursier, during the
restore, as the newest Temurin release of the JDK asked for. Each run's log records them (`java -version`); the set
below was labelled with Temurin 17.0.20.1 (the Maven builds) and 21.0.12.1 (the Gradle builds, whose Java
toolchains resolve to 17.0.20.1, 21.0.12.1 or the image's own 25.0.4.1).

scip-java compiles each repository with its own build: it runs javac and kotlinc as the build configures them, with
its plugins attached, so a name binds exactly where the compiler binds it. Main and test sources are both compiled.
[builds.txt](../../scripts/eval/identity/jvm/builds.txt) gives each repository's JDK and build arguments, and why any
differ from scip-java's defaults:

- **gson:** the `proto` module needs a newer Maven than the image has, so it isn't built.
- **commons-lang:** its parent POM compiles in-process unless told to fork, so scip-java's javac is passed to it
  through the parent's own properties.
- **moshi:** scip-kotlinc 0.13.2 runs inside Kotlin 2.4's compiler, and moshi pins Kotlin 2.3.21, so moshi is
  compiled with Kotlin 2.4.21 through its own `MOSHI_KOTLIN_VERSION` setting. Under 2.4, one Java test calls an
  internal Kotlin member by the name 2.3 gave it, so moshi's Java tests (`moshi/src/test/java`) aren't compiled. A
  Gradle init script ([no-werror.gradle](../../scripts/eval/identity/jvm/no-werror.gradle)) turns off warnings as
  errors, because Kotlin 2.4 warns about an option 2.3 needed. Warnings don't change what a name binds to.

**Source files:** every `.java` and `.kt` file the build compiles, outside build output (`build`, `target`, `out`,
`.gradle`) and generated sources (any `generated` directory: annotation processors' and KSP's output). A source file
the build doesn't compile (a Kotlin Multiplatform source set for another platform, a module left out above) gives no
references. label.py lists those files in its log.

#### What a reference is

As in v1. A Kotlin property that scip-kotlinc binds through its accessor (`rawType.get()`) is a reference to the
property, since the token names it. A qualified name (`java.util.List`) whose range covers its qualifier is a reference
at the name that ends it. For Java and Kotlin, left out also, each recorded in the enumeration with its reason (in
parentheses) and counted below:

- **Imports** (`import`): any occurrence on an `import` line. scip-java doesn't mark imports with SCIP's import role.
- **Class literals** (`class_literal`): the `class` of `Foo.class`, a keyword. `Foo` is a `type` reference.
- **Operators** (`operator`): a Kotlin operator's function, called with no token of its name (`!x` calls `not`,
  `x[i]` calls `get`, `a < b` calls `compareTo`, a `for` loop calls `iterator`, `hasNext` and `next`).
- **Constructors:** `<init>` (`new Foo(…)`, `Foo(…)` in Kotlin, `super(…)`, `this(…)`), and static initialisers.
  In a Kotlin supertype list (`class Owner : Person()`), scip-kotlinc binds the supertype's name to the class, so
  that name is a `type` reference.
- **Packages, type parameters, parameters and locals:** SCIP namespace (`a/b/`), type-parameter (`[T]`),
  parameter (`(x)`) and `local` symbols. A Kotlin named argument binds to its parameter, so it isn't labelled.
- **Renamed references** (`renamed_or_implicit`): the token differs from the declaration's own name. A Kotlin property
  read from Java through its getter (`getName()`), a Java getter read from Kotlin as a property (`name`), and an import
  alias are left out, as is a reference with no token at all (a `vararg` parameter's implicit `Array`).
- **Synthesised members** (`synthesised`): a member the compiler makes for one of the repository's types and declares
  nowhere in source: an enum's `values()` and `valueOf`, a record's accessors, a Kotlin data class's `copy` and
  `componentN`. A reference is one when its symbol has no definition and the type it belongs to is defined in the
  repository.
- **Unmatched overloads** (`overload_unmatched`): the symbol names a member the type declares, but under an overload
  disambiguator no definition has. scip-kotlinc logs these ("methodDisambiguator … not found"); the toolchain names
  no one declaration, so there is nothing to score against.
- **Generated targets** (`generated_target`): the target is declared only in generated sources.
- **References into the repository's own types from outside it** (`outside_own_type`): an outside symbol whose outermost
  type the repository declares. That is a type declared in a file the build didn't compile, or a published build of the
  repository's own code that a module depends on (moshi's `examples` module builds against moshi 1.14.0 from Maven
  Central). Where such a name binds says which version of the code is used, not which declaration the name means, so
  these references aren't labelled. The repository's declared types are read from every source file's text: its package,
  every name after `class`, `interface`, `object`, `enum`, `record` or `typealias`, the file's name and its Kotlin
  facade (`NameKt`, or its `@file:JvmName`).
- **Sites bound two ways** (`bound_two_ways`): as in v1, the draw leaves out a place the toolchain binds to two
  declarations.

Per language, the references the toolchain binds in the source files, how many are references by the rule, and how
many are left out by each reason ([exclusions.json](jvm/exclusions.json); `sample.py --exclusions` makes it from the
enumerations):

| Reason | Java | Kotlin |
| --- | ---: | ---: |
| References by the rule, the population the set is drawn from | 222031 | 40420 |
| `import` | 7721 | 3430 |
| `class_literal` | 6252 | 1 |
| `operator` | 0 | 2534 |
| `renamed_or_implicit` | 1 | 289 |
| `synthesised` | 179 | 136 |
| `overload_unmatched` | 10 | 400 |
| `generated_target` | 16 | 1 |
| `outside_own_type` | 2 | 7 |
| `bound_two_ways` | 0 | 7 |

#### Kinds

The first that applies:

1. `extension`: a Kotlin extension function or property, whose signature names a receiver type before its name
   (`fun String.shout()`). Java has none.
2. `dispatch`: an abstract member, a method declared by an interface (default methods included), or a property
   declared by a Kotlin interface. A member is abstract when scip-java gives it the abstract-method kind or its
   signature says `abstract` (scip-kotlinc gives Kotlin's abstract members no kind of their own). An annotation's
   elements are abstract methods but aren't dispatched, and a Java interface's field is a constant, a `member`.
3. `call`: a method or function, called or referenced (`Foo::bar`). An annotation's element named in a use
   (`@Mark(level = 1)`) is a `call`.
4. `member`: a field, enum constant or Kotlin property, read or written.
5. `type`: a class, interface, enum, record, annotation, Kotlin object or type alias named.

scip-java describes only the repository's own symbols. Outside the repository, a reference is classified by its
symbol's form alone: a method is a `call`, a term a `member`, a type a `type`. So no outside reference is
`dispatch` or `extension`.

#### The target

The declaration whose SCIP definition occurrence is the symbol's, at the line of its name token. An override is its
own declaration. A declaration defined in more than one compiled file lists each place: a Kotlin `expect`
declaration and its `actual` share one symbol, so both are listed.

Outside targets have an origin: `stdlib` for the JDK and Kotlin's standard library, `package` for any other library.
scip-java names a Java symbol's artifact, and the JDK's is `jdk`. It names no artifact for Kotlin symbols, so a
Kotlin symbol under `java/`, `jdk/` or `kotlin/` is `stdlib`. The origin is descriptive: the scorer treats every
outside label the same.

#### Multiplicity

As in v1: how many declarations in the repository, of both languages, have the target's simple name. Overloads are
separate declarations. Types, methods and functions, fields, properties and enum constants count; constructors,
type parameters, parameters and locals don't.

### Sampling

[sample.py](../../scripts/eval/identity/sample.py), as in v1, with strata per repository and language. Java has no
`extension` kind, and neither language has an outside `dispatch` or `extension` reference, so those strata are
empty.

### The set

2686 references: 1501 Java (1118 inside, 383 outside) over four repositories, 1185 Kotlin (899 inside, 286 outside)
over three. Per repository: commons-lang 524, gson 481, moshi 280 Java and 452 Kotlin, spring-petclinic 216, kotlinpoet
557, spring-petclinic-kotlin 176. Per language, kind and stratum (inside by bucket, outside by bucket):

| Language | Kind | inside 1 | inside 2-4 | inside 5+ | outside 0 | outside 1+ | All |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Java | dispatch | 31 | 62 | 80 | 0 | 0 | 173 |
| Java | call | 80 | 124 | 120 | 40 | 152 | 516 |
| Java | member | 80 | 141 | 143 | 40 | 64 | 468 |
| Java | type | 80 | 94 | 83 | 40 | 47 | 344 |
| Kotlin | extension | 40 | 80 | 51 | 0 | 0 | 171 |
| Kotlin | dispatch | 19 | 44 | 49 | 0 | 0 | 112 |
| Kotlin | call | 60 | 82 | 80 | 30 | 88 | 340 |
| Kotlin | member | 60 | 90 | 120 | 30 | 80 | 380 |
| Kotlin | type | 60 | 31 | 33 | 30 | 28 | 182 |

A stratum short of its quota held fewer references, or fewer distinct targets under the spread limit. The six
repositories were built twice, by two separate runs of run.sh, and the two builds' enumerations hold the same
references, targets and origins. The committed enumerations' digests are of label.py's output over the second run's
SCIP indexes.

**Corrected after the first scoring run.** For 7 Java labels, scip-java marks the declared name's occurrence inside
the declaration's own annotation as its definition (`@JsonAdapter(A.Adapter.class)` above `class A`, a field under
`@Value("…")`), so their target was the annotation's line, not the name token's that the rule names. The first run
over the set, with KnowCode's extraction and no Java resolver, judged those 7 rows wrong though KnowCode named the
declaration the toolchain binds. label.py now takes a definition on a line that starts with an annotation to the next
line, within 10, with the name as a word outside annotations. Re-run over the same SCIP indexes, it moved exactly
those 7 labels' targets (gson's `A` ×3, `WithJsonDeserializer` and `DummyClass` ×2, spring-petclinic's `port`), each to
the following line; the draw and every other label are unchanged.

### Scoring

[identity_gate.py](../../scripts/eval/identity_gate.py), as in v1, with this set's labels:

```sh
python3 scripts/eval/identity_gate.py fetch --work WORK --repos gson,commons-lang,spring-petclinic,moshi,kotlinpoet,spring-petclinic-kotlin
python3 scripts/eval/identity_gate.py run --knowcode BIN --work WORK --labels eval/identity-v2/jvm/labels.jsonl --out jvm.json
python3 scripts/eval/identity_gate.py check jvm.json
```

### Running it

```sh
scripts/eval/identity/run.sh REPO CLONE IO      # each repository; Docker; the enumeration, log and SCIP index come out, in IO/out
python3 scripts/eval/identity/sample.py OUT/enumeration-*.jsonl.gz --out eval/identity-v2/jvm/labels.jsonl \
  --strata eval/identity-v2/jvm/strata.json --exclusions eval/identity-v2/jvm/exclusions.json
```

## Go

The labels are in [go/](go/labels.jsonl): 1,194 over three repositories (cobra 242, chi 418, gin 534), 705 inside and
489 outside, per stratum in [go/strata.json](go/strata.json). Two runs of each repository give byte-identical
enumerations ([go/enumerations.sha256](go/enumerations.sha256)).

### Repositories

| Repository | Commit |
| --- | --- |
| spf13/cobra | `adbc8813901bba65827259daa8e22ff94ec1f30e` |
| go-chi/chi | `9f8adb1aabaa8a91180a821111d3f8e668d1c0bf` |
| gin-gonic/gin | `0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646` |

Pinned in [repos.txt](../../scripts/eval/identity/repos.txt) at each default branch's head on 2026-10-29, before
anything was labelled.

### Labelling

[go/label.go](../../scripts/eval/identity/go/label.go) loads every module in the repository (each `go.mod` outside
`vendor`, `testdata` and hidden directories) with its tests, through `golang.org/x/tools/go/packages`, and reads
`go/types`' `Uses` and `Defs`: the type checker that gopls and scip-go are both built on. scip-go was tried first and
dropped because its index carries no information about outside symbols, so a call to an outside interface's method
(`io.Writer.Write`) couldn't be told from a call to a function.

| Image | Toolchain |
| --- | --- |
| `golang:1.27-bookworm` (`sha256:d9b6a1be…`) | Go 1.27.3, `golang.org/x/tools` v0.52.0 |

The network is up while each module's dependencies (`go mod download`, then `go list -test -deps ./...`) and the
labeller's own are fetched, and down while it type-checks (`GOPROXY=off`). Only the default build is read: Linux,
`CGO_ENABLED=1`, and no build tags, so a file excluded by its build constraints is neither labelled nor scored.

The source files are the repository's own `.go` files, tests included. Files under `vendor/` and files cgo produces
are left out. Generated files (a `// Code generated … DO NOT EDIT.` header) give no references, but a declaration in
one is inside.

A repository may hold more than one module: go-chi/chi's `_examples/rest` and `_examples/versions` are modules of
their own. Each is type-checked as `go build` would see it. Where such a module requires a released version of the
repository's own module (`github.com/go-chi/chi/v5 v5.2.4`) without a `replace`, the toolchain binds its references
to the module cache's copy, not the repository's code, so they are **outside** (`package`): linking them to the
repository's declaration of the same name would be the wrong link this gate exists to catch. With a `replace` of the
module by a directory of the repository (`=> ../`), they are inside. chi has 92 such outside references; cobra and gin
have none, and the resolver's tests cover the `replace` case.

The first version of this section (commit `7a46a8c`) left these 92 references out as neither inside nor outside. They
were counted as outside on the maintainer's decision of 2026-10-29, after KnowCode's answers on the first draw had
been read; the change can only add outside labels, which the gate requires KnowCode not to link.

### What a reference is, in Go

The rule above, with these Go cases:

- **Left out:** package names (import specs and qualifiers such as the `http` in `http.Get`), builtins (`len`,
  `error`, `nil`), labels, locals, parameters, results, type parameters, types declared inside a function and their
  fields (a table test's cases), the blank identifier, and anything in the cgo pseudo-package `C`.
- **A composite literal** `T{…}` is a reference to the type `T`. Go has no constructors.
- **Embedded fields:** the type named in an embedding is a `type` reference. A promoted field or method used through
  the outer type (`outer.Inner()`) binds to the embedded type's declaration, and that is the label.
- **Generics:** an instantiated function, method or field (`Map[int]`, `List[T].Push`) is labelled with its generic
  definition (`types.Func.Origin`, `types.Var.Origin`).
- **A test package** (`package x_test`) and the package it tests are type-checked together, as `go test` does, so a
  test's reference to the package under test is inside.

### Kinds, in Go

Go has no extensions, so `extension` never occurs.

1. `dispatch`: a method declared in an interface. A call through an interface value binds to the interface's
   method, never to a concrete type's.
2. `call`: a function or a method declared on a concrete type, called or used as a value.
3. `member`: a struct field, or a package-level variable or constant.
4. `type`: a named type or type alias declared at package level.

### Exclusions

Every reference the checker binds that the rule leaves out of the repository's own references is written to the
enumeration as an `excluded` row with its reason, and [go/exclusions.json](go/exclusions.json) counts them:
`local_type_field` (a field of a struct type declared inside a function: 1,987), `generated` (a reference in a
generated file) and `cgo` (a name in the pseudo-package `C`); the last two don't occur in these repositories. Locals,
parameters, builtins and package names aren't bindings to a declaration the rule could label, and aren't counted.

### Targets, origins and multiplicity

A target is the declaration's name token: its file and line. The key is that token's file, line and column, which
are the same in a package's test and non-test variants. An outside target's origin is `stdlib` when the first element
of its package path has no dot (`net/http`, `context`), and `package` otherwise (a module dependency). Go has no
`sdk`.

Multiplicity counts the repository's declarations of the target's name that the checker saw: package-level types,
functions and methods (interface methods included), fields of package-level struct types, and package-level variables
and constants.

### Cross-check against scip-go

label.go is this project's own code, so its targets are checked against scip-go v0.2.9's, which reads the same type
checker through code of its own. run.sh indexes every module with scip-go in the same container, offline, and
[go/crosscheck.py](../../scripts/eval/identity/go/crosscheck.py) compares each reference both report at the same place.
An inside target agrees when scip-go's symbol is defined at the same file and line; an outside one when scip-go's
symbol isn't defined in the repository and names the same package and name.

| Repository | References | Also reported by scip-go | Agree | Labels | Labels compared | Labels agreeing |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| cobra | 8,461 | 8,461 | 8,461 | 242 | 242 | 242 |
| chi | 7,424 | 7,424 | 7,424 | 418 | 418 | 418 |
| gin | 17,538 | 17,532 | 17,513 | 534 | 533 | 532 |
| All | 33,423 | 33,417 | 33,398 (99.94%) | 1,194 | 1,193 | 1,192 |

chi's 92 references to its released copy agree: scip-go binds them outside too.

Every difference, all in gin, has one of two causes, and neither is a labelling error:

- **19 references to gin's `testdata/protoexample/test.pb.go`** (`Test` and its fields): scip-go indexes `./...`,
  which skips `testdata` directories, so it has no definition there. The file is the repository's own, so the
  labeller's inside target stands. One of these is a label.
- **6 references to `unsafe.Pointer`**: scip-go records no occurrence for the `unsafe` package's members. One is a
  label.

| Reference | Name | Labeller's target | scip-go |
| --- | --- | --- | --- |
| `binding/binding_test.go:1346:22` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:1351:30` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:1353:21` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:1409:22` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:1424:21` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:715:24` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:716:3` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:727:24` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/binding_test.go:728:3` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `binding/json_test.go:133:40` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `binding/json_test.go:138:39` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `binding/json_test.go:147:39` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `binding/json_test.go:189:48` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `binding/json_test.go:194:47` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `binding/json_test.go:203:47` | `Pointer` | `unsafe.Pointer` (stdlib) | no occurrence |
| `context_test.go:1736:20` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `context_test.go:1737:3` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `context_test.go:1738:3` | `Reps` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `context_test.go:1902:20` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `context_test.go:1903:3` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `context_test.go:1904:3` | `Reps` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `render/render_test.go:371:20` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `render/render_test.go:372:3` | `Label` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `render/render_test.go:373:3` | `Reps` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |
| `render/render_test.go:390:20` | `Test` | `testdata/protoexample/test.pb.go` | no definition (testdata not indexed) |

### Held out: labstack/echo

The resolver was developed against the three repositories above, so it is also scored once on one it wasn't:
labstack/echo at `4deac5da0714abbc2037a90067ea82c033a3f266`, the reserve chosen with them. Its labels
([go/heldout/labels.jsonl](go/heldout/labels.jsonl): 508, 308 inside and 200 outside) were drawn by the same rule,
labeller and sampler, and committed before KnowCode answered on them. They are scored once, by the build whose
resolver was tuned on the three, and the result is reported as it is: no change to the resolver is made for them.
Two runs give byte-identical enumerations; scip-go agrees on all 24,933 references both report.

### Running it, Go

```sh
scripts/eval/identity/run.sh REPO CLONE IO      # cobra, chi, gin; the enumeration, log and scip-go indexes, in IO/out
python3 scripts/eval/identity/go/crosscheck.py IO/out/enumeration-REPO.jsonl.gz IO/out/scip-REPO \
  --labels eval/identity-v2/go/labels.jsonl --list
python3 scripts/eval/identity/sample.py OUT/enumeration-*.jsonl.gz --out eval/identity-v2/go/labels.jsonl \
  --strata eval/identity-v2/go/strata.json --exclusions eval/identity-v2/go/exclusions.json
python3 scripts/eval/identity_gate.py run --knowcode BIN --work WORK --labels eval/identity-v2/go/labels.jsonl \
  --repos cobra,chi,gin --out go.json
```
