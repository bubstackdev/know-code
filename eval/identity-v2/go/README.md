# Identity against a compiler: Go

identity-v2's Go labels ([labels.jsonl](labels.jsonl)), for 0.7's gate item "Identity, Go, Java and Kotlin"
([plan](../../../docs/plans/0.7.md#gate)). identity-v2 keeps [identity-v1](../../identity-v1/README.md)'s rule: what a
reference is, its kind, its target, the multiplicity buckets, the draw and the scoring. This file states only what
Go adds or reads differently. It was committed with the labels, before any KnowCode answer on them was read, and it
doesn't change to fit them; it moves into identity-v2's shared README unchanged when that lands.

The draw: 1,163 labels over three repositories (cobra 242, chi 387, gin 534), per stratum in
[strata.json](strata.json); [enumerations.sha256](enumerations.sha256) holds the digests of the enumerations drawn
from, which aren't checked in. Two runs of each repository give byte-identical enumerations.


## Repositories

| Repository | Commit |
| --- | --- |
| spf13/cobra | `adbc8813901bba65827259daa8e22ff94ec1f30e` |
| go-chi/chi | `9f8adb1aabaa8a91180a821111d3f8e668d1c0bf` |
| gin-gonic/gin | `0f09c3a9b4626d9fe9979cf4d0521d6ae32ab646` |

Pinned in [repos.txt](../../../scripts/eval/identity/repos.txt) at each default branch's head on 2026-10-29, before
anything was labelled.

## Labelling

[label.go](../../../scripts/eval/identity/go/label.go) loads every module in the repository (each `go.mod` outside
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
to the module cache's copy: the same code at another version. Neither inside nor outside is a fair label for that, so
those references are left out, and the labeller's log counts them (92 in chi, none in cobra or gin).

## What a reference is, in Go

The rule above, with these Go cases:

- **Left out:** references to a published copy of the repository's own module (above), package names (import specs
  and qualifiers such as the `http` in `http.Get`), builtins (`len`, `error`, `nil`), labels, locals, parameters,
  results, type parameters, types declared inside a function and their fields (a table test's cases), the blank
  identifier, and anything in the cgo pseudo-package `C`.
- **A composite literal** `T{…}` is a reference to the type `T`. Go has no constructors.
- **Embedded fields:** the type named in an embedding is a `type` reference. A promoted field or method used through
  the outer type (`outer.Inner()`) binds to the embedded type's declaration, and that is the label.
- **Generics:** an instantiated function, method or field (`Map[int]`, `List[T].Push`) is labelled with its generic
  definition (`types.Func.Origin`, `types.Var.Origin`).
- **A test package** (`package x_test`) and the package it tests are type-checked together, as `go test` does, so a
  test's reference to the package under test is inside.

## Kinds, in Go

Go has no extensions, so `extension` never occurs.

1. `dispatch`: a method declared in an interface. A call through an interface value binds to the interface's
   method, never to a concrete type's.
2. `call`: a function or a method declared on a concrete type, called or used as a value.
3. `member`: a struct field, or a package-level variable or constant.
4. `type`: a named type or type alias declared at package level.

## Targets, origins and multiplicity

A target is the declaration's name token: its file and line. The key is that token's file, line and column, which
are the same in a package's test and non-test variants. An outside target's origin is `stdlib` when the first element
of its package path has no dot (`net/http`, `context`), and `package` otherwise (a module dependency). Go has no
`sdk`.

Multiplicity counts the repository's declarations of the target's name that the checker saw: package-level types,
functions and methods (interface methods included), fields of package-level struct types, and package-level variables
and constants.

## Cross-check against scip-go

label.go is this project's own code, so its targets are checked against scip-go v0.2.9's, which reads the same type
checker through code of its own. run.sh indexes every module with scip-go in the same container, offline, and
[crosscheck.py](../../../scripts/eval/identity/go/crosscheck.py) compares each reference both report at the same place.
An inside target agrees when scip-go's symbol is defined at the same file and line; an outside one when scip-go's
symbol isn't defined in the repository and names the same package and name.

| Repository | References | Also reported by scip-go | Agree | Labels | Labels compared | Labels agreeing |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| cobra | 8,461 | 8,461 | 8,461 | 242 | 242 | 242 |
| chi | 7,332 | 7,332 | 7,332 | 387 | 387 | 387 |
| gin | 17,538 | 17,532 | 17,513 | 534 | 533 | 532 |
| All | 33,331 | 33,325 | 33,306 (99.94%) | 1,163 | 1,162 | 1,161 |

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
