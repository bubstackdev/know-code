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
there were and how many were drawn) and `enumerations.sha256` (the digests of the enumerations drawn from, which
aren't checked in).

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

As in v1. For Java and Kotlin, left out also:

- **Imports:** any occurrence on an `import` line. scip-java doesn't mark imports with SCIP's import role.
- **Constructors:** `<init>` (`new Foo(…)`, `Foo(…)` in Kotlin, `super(…)`, `this(…)`), and static initialisers.
  In a Kotlin supertype list (`class Owner : Person()`), scip-kotlinc binds the supertype's name to the class, so
  that name is a `type` reference.
- **Packages, type parameters, parameters and locals:** SCIP namespace (`a/b/`), type-parameter (`[T]`),
  parameter (`(x)`) and `local` symbols. A Kotlin named argument binds to its parameter, so it isn't labelled.
- **Renamed references:** the token differs from the declaration's own name. A Kotlin property read from Java through
  its getter (`getName()`), a Java getter read from Kotlin as a property (`name`), and an import alias are left out.
- **Synthesised members:** a member the compiler makes for one of the repository's types and declares nowhere in
  source: an enum's `values()` and `valueOf`, a record's accessors, a Kotlin data class's `copy` and `componentN`.
  A reference is one when its symbol has no definition and the type it belongs to is defined in the repository.
- **References into the repository's own types from outside it:** an outside symbol whose outermost type the
  repository declares. That is a type declared in a file the build didn't compile, or a published build of the
  repository's own code that a module depends on (moshi's `examples` module builds against moshi 1.14.0 from Maven
  Central). Where such a name binds says which version of the code is used, not which declaration the name means,
  so these references aren't labelled. The repository's declared types are read from every source file's text: its
  package, every name after `class`, `interface`, `object`, `enum`, `record` or `typealias`, the file's name and its
  Kotlin facade (`NameKt`, or its `@file:JvmName`). label.py counts what this leaves out in its log.

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
own declaration. A declaration defined in more than one compiled file lists each place.

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

2673 references: 1516 Java (1118 inside, 398 outside) over four repositories, 1157 Kotlin (890 inside, 267 outside)
over three. Per repository: commons-lang 524, gson 481, moshi 295 Java and 440 Kotlin, spring-petclinic 216, kotlinpoet
541, spring-petclinic-kotlin 176. Per language, kind and stratum (inside by bucket, outside by bucket):

| Language | Kind | inside 1 | inside 2-4 | inside 5+ | outside 0 | outside 1+ | All |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Java | dispatch | 31 | 62 | 80 | 0 | 0 | 173 |
| Java | call | 80 | 124 | 120 | 40 | 152 | 516 |
| Java | member | 80 | 141 | 143 | 40 | 79 | 483 |
| Java | type | 80 | 94 | 83 | 40 | 47 | 344 |
| Kotlin | extension | 40 | 80 | 51 | 0 | 0 | 171 |
| Kotlin | dispatch | 19 | 44 | 49 | 0 | 0 | 112 |
| Kotlin | call | 60 | 82 | 80 | 30 | 88 | 340 |
| Kotlin | member | 60 | 90 | 120 | 30 | 80 | 380 |
| Kotlin | type | 60 | 28 | 27 | 30 | 9 | 154 |

A stratum short of its quota held fewer references, or fewer distinct targets under the spread limit. The six
repositories were labelled twice, by two separate runs of run.sh, and the two runs' enumerations hold the same
references, targets and origins.

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
  --strata eval/identity-v2/jvm/strata.json
```
