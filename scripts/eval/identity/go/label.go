// Command label enumerates one Go repository for identity-v2 (eval/identity-v2/README.md): every reference the Go
// type checker binds in the repository's own source, and every declaration it sees there, as JSON lines in the
// enumeration format sample.py reads.
//
//	label -repo NAME -src DIR
//
// Every module under DIR (each go.mod outside vendor and testdata) is loaded with its tests through
// golang.org/x/tools/go/packages, which type-checks with go/types: the checker gopls and scip-go are built on.
// A package loaded more than once (once alone and once with its in-package tests) is read once per file.
package main

import (
	"bufio"
	"encoding/json"
	"flag"
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"golang.org/x/mod/modfile"
	"golang.org/x/tools/go/packages"
)

// declRow and refRow are the enumeration's two row types (sample.py). A reference always carries `locs` and
// `origin`: [] and an origin for an outside target, its declaration's place and null for an inside one.
type declRow struct {
	T    string `json:"t"`
	Repo string `json:"repo"`
	File string `json:"file"`
	Line int    `json:"line"`
	Name string `json:"name"`
	Key  string `json:"key"`
	Decl string `json:"decl"`
}

type refRow struct {
	T      string   `json:"t"`
	Repo   string   `json:"repo"`
	File   string   `json:"file"`
	Line   int      `json:"line"`
	Col    int      `json:"col"`
	Name   string   `json:"name"`
	Key    string   `json:"key"`
	Kind   string   `json:"kind"`
	Inside bool     `json:"inside"`
	Locs   [][2]any `json:"locs"`
	Origin *string  `json:"origin"`
	Symbol string   `json:"symbol"`
	Called bool     `json:"called"`
}

func main() {
	repo := flag.String("repo", "", "repository name (repos.txt)")
	src := flag.String("src", "", "the repository's checkout")
	flag.Parse()
	if *repo == "" || *src == "" {
		fmt.Fprintln(os.Stderr, "usage: label -repo NAME -src DIR")
		os.Exit(2)
	}
	root, err := filepath.Abs(*src)
	if err != nil {
		fatal(err)
	}
	e := &enumerator{repo: *repo, root: root, decls: map[string]declRow{}, refs: map[string]refRow{}, generated: map[string]bool{}}
	dirs := modules(root)
	e.own = modulePaths(dirs)
	for _, dir := range dirs {
		e.load(dir)
	}
	e.write(os.Stdout)
	fmt.Fprintf(os.Stderr, "label: %d references to a published copy of the repository's own modules left out\n", e.ownCopies)
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "label:", err)
	os.Exit(1)
}

// modules lists the directories holding a go.mod, outside vendor, testdata and hidden directories.
func modules(root string) []string {
	var out []string
	filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if d.IsDir() && path != root && (d.Name() == "vendor" || d.Name() == "testdata" || strings.HasPrefix(d.Name(), ".")) {
			return filepath.SkipDir
		}
		if !d.IsDir() && d.Name() == "go.mod" {
			out = append(out, filepath.Dir(path))
		}
		return nil
	})
	sort.Strings(out)
	return out
}

type enumerator struct {
	repo, root string
	decls      map[string]declRow // by key
	refs       map[string]refRow  // by file:line:col:key
	generated  map[string]bool
	fset       *token.FileSet
	own        []string // module paths the repository's go.mod files declare
	ownCopies  int
	bodies     map[string][][2]token.Pos // function bodies by file name, for fields of types declared in them
}

func (e *enumerator) load(dir string) {
	e.fset = token.NewFileSet()
	cfg := &packages.Config{
		Mode: packages.NeedName | packages.NeedFiles | packages.NeedCompiledGoFiles | packages.NeedSyntax |
			packages.NeedTypes | packages.NeedTypesInfo | packages.NeedModule,
		Dir:   dir,
		Tests: true,
		Fset:  e.fset,
	}
	pkgs, err := packages.Load(cfg, "./...")
	if err != nil {
		fatal(err)
	}
	packages.Visit(pkgs, nil, func(p *packages.Package) {
		for _, err := range p.Errors {
			fmt.Fprintf(os.Stderr, "label: %s: %v\n", p.PkgPath, err)
		}
	})
	e.bodies = map[string][][2]token.Pos{}
	for _, p := range pkgs {
		for _, f := range p.Syntax {
			name := e.fset.File(f.Pos()).Name()
			ast.Inspect(f, func(n ast.Node) bool {
				var body *ast.BlockStmt
				switch fn := n.(type) {
				case *ast.FuncDecl:
					body = fn.Body
				case *ast.FuncLit:
					body = fn.Body
				}
				if body != nil {
					e.bodies[name] = append(e.bodies[name], [2]token.Pos{body.Pos(), body.End()})
					return false // a nested body lies inside this one
				}
				return true
			})
		}
	}
	for _, p := range pkgs {
		if p.TypesInfo == nil {
			continue
		}
		for _, f := range p.Syntax {
			rel, ok := e.rel(e.fset.File(f.Pos()).Name())
			if !ok {
				continue // cgo output and other files outside the checkout
			}
			if ast.IsGenerated(f) {
				e.generated[rel] = true
			}
			e.file(p, f, rel)
		}
	}
}

// modulePaths reads the module path each go.mod declares.
func modulePaths(dirs []string) []string {
	var out []string
	for _, dir := range dirs {
		data, err := os.ReadFile(filepath.Join(dir, "go.mod"))
		if err != nil {
			fatal(err)
		}
		if path := modfile.ModulePath(data); path != "" {
			out = append(out, path)
		}
	}
	return out
}

func (e *enumerator) ownModule(pkg string) bool {
	for _, m := range e.own {
		if pkg == m || strings.HasPrefix(pkg, m+"/") {
			return true
		}
	}
	return false
}

// rel is a file's repository-relative path, when it is one of the repository's own files.
func (e *enumerator) rel(abs string) (string, bool) {
	r, err := filepath.Rel(e.root, abs)
	if err != nil || strings.HasPrefix(r, "..") || !strings.HasSuffix(r, ".go") {
		return "", false
	}
	for _, part := range strings.Split(filepath.ToSlash(r), "/") {
		if part == "vendor" {
			return "", false
		}
	}
	return filepath.ToSlash(r), true
}

func (e *enumerator) file(p *packages.Package, f *ast.File, rel string) {
	info := p.TypesInfo
	called := map[*ast.Ident]bool{}
	ast.Inspect(f, func(n ast.Node) bool {
		if c, ok := n.(*ast.CallExpr); ok {
			switch fn := ast.Unparen(c.Fun).(type) {
			case *ast.Ident:
				called[fn] = true
			case *ast.SelectorExpr:
				called[fn.Sel] = true
			case *ast.IndexExpr: // f[T](…)
				if id := last(fn.X); id != nil {
					called[id] = true
				}
			case *ast.IndexListExpr:
				if id := last(fn.X); id != nil {
					called[id] = true
				}
			}
		}
		return true
	})
	for id, obj := range info.Defs {
		if obj == nil {
			continue
		}
		if group := e.declGroup(obj); group != "" {
			pos := e.fset.Position(id.Pos())
			if r, ok := e.rel(pos.Filename); ok && r == rel {
				k := key(obj, e)
				if _, seen := e.decls[k]; !seen {
					e.decls[k] = declRow{T: "decl", Repo: e.repo, Name: obj.Name(), Key: k, File: rel, Line: pos.Line, Decl: group}
				}
			}
		}
	}
	for id, obj := range info.Uses {
		kind := e.refKind(origin(obj))
		if kind == "" || id.Name != obj.Name() {
			continue
		}
		pos := e.fset.Position(id.Pos())
		if r, ok := e.rel(pos.Filename); !ok || r != rel {
			continue // TypesInfo covers the whole package; this pass reads one file
		}
		obj = origin(obj)
		k := key(obj, e)
		r := refRow{T: "ref", Repo: e.repo, File: rel, Line: pos.Line, Col: pos.Column, Name: obj.Name(), Key: k,
			Kind: kind, Symbol: e.symbol(obj), Called: called[id], Locs: [][2]any{}}
		dpos := e.fset.Position(obj.Pos())
		if drel, ok := e.rel(dpos.Filename); ok && obj.Pos().IsValid() {
			r.Inside = true
			r.Locs = [][2]any{{drel, dpos.Line}}
		} else if e.ownModule(obj.Pkg().Path()) {
			// A nested module that requires a released version of the repository's own module, without a replace,
			// binds to the module cache's copy: the same code at another version, so neither inside nor outside.
			e.ownCopies++
			continue
		} else {
			o := originOf(obj)
			r.Origin = &o
		}
		e.refs[fmt.Sprintf("%s:%d:%d:%s", rel, pos.Line, pos.Column, k)] = r
	}
}

func last(x ast.Expr) *ast.Ident {
	switch v := ast.Unparen(x).(type) {
	case *ast.Ident:
		return v
	case *ast.SelectorExpr:
		return v.Sel
	}
	return nil
}

// origin is the generic definition behind an instantiated function, method or field.
func origin(obj types.Object) types.Object {
	switch o := obj.(type) {
	case *types.Func:
		return o.Origin()
	case *types.Var:
		return o.Origin()
	}
	return obj
}

// packageLevel reports a variable or constant declared at package scope (not a local or a parameter).
func packageLevel(obj types.Object) bool {
	return obj.Pkg() != nil && obj.Parent() == obj.Pkg().Scope()
}

// inBody reports a declaration inside a function body: a field of a struct type declared there (a table test's
// cases) is a local, as the README's rule has it.
func (e *enumerator) inBody(obj types.Object) bool {
	if !obj.Pos().IsValid() {
		return false
	}
	f := e.fset.File(obj.Pos())
	if f == nil {
		return false
	}
	for _, b := range e.bodies[f.Name()] {
		if b[0] <= obj.Pos() && obj.Pos() < b[1] {
			return true
		}
	}
	return false
}

// declGroup is the multiplicity group of a declaration counted by name, or "" for one that isn't counted: locals,
// parameters, results, type parameters, fields of types declared in a function, labels, package names and blank
// identifiers.
func (e *enumerator) declGroup(obj types.Object) string {
	if obj.Name() == "_" || obj.Pkg() == nil {
		return ""
	}
	switch o := obj.(type) {
	case *types.TypeName:
		if _, tp := o.Type().(*types.TypeParam); tp || !(packageLevel(o)) {
			return ""
		}
		return "type"
	case *types.Func:
		return "callable"
	case *types.Var:
		if (o.IsField() && !e.inBody(o)) || packageLevel(o) {
			return "member"
		}
	case *types.Const:
		if packageLevel(o) {
			return "member"
		}
	}
	return ""
}

// refKind is a reference's kind (README "Kinds"), or "" for one that isn't labelled: package names (qualifiers and
// imports), builtins, nil, labels, locals, parameters and type parameters, and local types and their fields.
func (e *enumerator) refKind(obj types.Object) string {
	if obj.Pkg() == nil || obj.Pkg().Path() == "C" || obj.Name() == "_" {
		return ""
	}
	switch o := obj.(type) {
	case *types.Func:
		if recv := o.Signature().Recv(); recv != nil {
			if _, ok := recv.Type().Underlying().(*types.Interface); ok {
				return "dispatch"
			}
		}
		return "call"
	case *types.Var:
		if (o.IsField() && !e.inBody(o)) || packageLevel(o) {
			return "member"
		}
	case *types.Const:
		if packageLevel(o) {
			return "member"
		}
	case *types.TypeName:
		if _, tp := o.Type().(*types.TypeParam); tp || !packageLevel(o) {
			return ""
		}
		return "type"
	}
	return ""
}

// key identifies a declaration across the packages it is loaded in: its definition's position, which is the same in
// a package's test and non-test variants. An object with no position (unsafe's) falls back to its symbol.
func key(obj types.Object, e *enumerator) string {
	if !obj.Pos().IsValid() {
		return e.symbol(obj)
	}
	pos := e.fset.Position(obj.Pos())
	if r, ok := e.rel(pos.Filename); ok {
		return fmt.Sprintf("%s:%d:%d", r, pos.Line, pos.Column)
	}
	return e.symbol(obj)
}

// symbol is a readable, stable name: the package path, the receiver or field owner when there is one, and the name.
func (e *enumerator) symbol(obj types.Object) string {
	path := obj.Pkg().Path()
	switch o := obj.(type) {
	case *types.Func:
		if recv := o.Signature().Recv(); recv != nil {
			return path + "." + typeName(recv.Type()) + "." + o.Name()
		}
	case *types.Var:
		if o.IsField() {
			// A field has no qualified name of its own (its struct may be anonymous), so its place tells it apart:
			// the file's base name, line and column, which don't depend on the order packages are loaded in.
			pos := e.fset.Position(o.Pos())
			return fmt.Sprintf("%s.(field).%s@%s:%d:%d", path, o.Name(), filepath.Base(pos.Filename), pos.Line, pos.Column)
		}
	}
	return path + "." + obj.Name()
}

func typeName(t types.Type) string {
	if p, ok := t.(*types.Pointer); ok {
		t = p.Elem()
	}
	switch n := types.Unalias(t).(type) {
	case *types.Named:
		return n.Obj().Name()
	}
	return types.TypeString(t, func(*types.Package) string { return "" })
}

// originOf says where an outside declaration comes from: the standard library, or a module the repository depends on.
func originOf(obj types.Object) string {
	path := obj.Pkg().Path()
	first := strings.SplitN(path, "/", 2)[0]
	if !strings.Contains(first, ".") {
		return "stdlib"
	}
	return "package"
}

func (e *enumerator) write(w *os.File) {
	out := bufio.NewWriter(w)
	defer out.Flush()
	enc := json.NewEncoder(out)
	keys := make([]string, 0, len(e.decls))
	for k := range e.decls {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		enc.Encode(e.decls[k])
	}
	keys = keys[:0]
	for k := range e.refs {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		r := e.refs[k]
		if e.generated[r.File] {
			continue // README: generated files are not the repository's source
		}
		enc.Encode(r)
	}
}
