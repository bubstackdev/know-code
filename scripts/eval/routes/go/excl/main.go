// Command excl lists the configuration-dependent regions of a Go program's code, by routes-v1's exclusion rule
// (eval/routes-v1/README.md), read with the type checker from the clone alone: it never reads KnowCode's output. A
// registration site inside a listed region is configuration-dependent.
//
//	excl -module DIR -package PKG [-test] [-overlay FILE]
//
// PKG is the program's main package, relative to the module at DIR, or with -test the package whose harness test
// (TestKCRoutes) builds the router; FILE is the build's overlay, so that the harness test is read too. It prints one
// JSON line per region: the file (absolute), its first and last line, and why:
//
//   - `condition`: the body of an if or else, or a case of a switch or select, in any function of the program's
//     packages (a loop is not a condition);
//   - `called under a condition`: the body of a function every reference to which lies in such a condition of the
//     function making it;
//   - `unreferenced`: the body of a function nothing in the program's packages refers to.
//
// The entry point (main, or the harness test) and init functions are never the last two. A reference is a call, or a
// function or method value; a call through an interface method refers to every method of that name declared in the
// program's packages. Only one hop is taken: a function called unconditionally from a function that is itself called
// only under a condition is not configuration-dependent, so a command-line dispatch (`case "serve":`) doesn't make
// the whole server's routes so.
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"go/ast"
	"go/token"
	"go/types"
	"os"
	"sort"
	"strings"

	"golang.org/x/tools/go/packages"
)

type region struct {
	File   string `json:"file"`
	Start  int    `json:"start"`
	End    int    `json:"end"`
	Reason string `json:"reason"`
}

// fn is a function body: a declaration or a literal.
type fn struct {
	pkg  *packages.Package
	body *ast.BlockStmt
	node ast.Node
}

func main() {
	module := flag.String("module", ".", "the module directory")
	pkgPath := flag.String("package", ".", "the program's package, relative to the module")
	test := flag.Bool("test", false, "the program is the package's harness test TestKCRoutes")
	root := flag.String("root", "/work/src", "the repository's directory: only its packages are the program's own")
	overlay := flag.String("overlay", "", "the build's overlay file (go build -overlay), so the harness test is read")
	flag.Parse()

	files := map[string][]byte{}
	if *overlay != "" {
		var o struct{ Replace map[string]string }
		b, err := os.ReadFile(*overlay)
		if err == nil {
			err = json.Unmarshal(b, &o)
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, "excl:", err)
			os.Exit(1)
		}
		for to, from := range o.Replace {
			if c, err := os.ReadFile(from); err == nil {
				files[to] = c
			}
		}
	}

	cfg := &packages.Config{
		Mode:  packages.NeedName | packages.NeedFiles | packages.NeedSyntax | packages.NeedTypes | packages.NeedTypesInfo | packages.NeedDeps | packages.NeedImports | packages.NeedModule,
		Dir:   *module,
		Tests:   *test,
		Overlay: files,
	}
	roots, err := packages.Load(cfg, "./"+strings.TrimPrefix(*pkgPath, "./"))
	if err != nil {
		fmt.Fprintln(os.Stderr, "excl:", err)
		os.Exit(1)
	}
	if packages.PrintErrors(roots) > 0 {
		os.Exit(1)
	}

	// The program's own packages: those whose files are in the repository (another module of it included, for one
	// the program's module replaces with a directory of the repository).
	var mine []*packages.Package
	seen := map[string]bool{}
	packages.Visit(roots, nil, func(p *packages.Package) {
		if seen[p.ID] || len(p.GoFiles) == 0 || !strings.HasPrefix(p.GoFiles[0], strings.TrimSuffix(*root, "/")+"/") {
			return
		}
		seen[p.ID] = true
		mine = append(mine, p)
	})

	// Every function body in the program's packages, by its declaration's object (literals by node).
	byObj := map[types.Object]*fn{}
	var all []*fn
	methodsByName := map[string][]*fn{}
	for _, p := range mine {
		for _, f := range p.Syntax {
			ast.Inspect(f, func(n ast.Node) bool {
				switch d := n.(type) {
				case *ast.FuncDecl:
					if d.Body == nil {
						return true
					}
					x := &fn{pkg: p, body: d.Body, node: d}
					all = append(all, x)
					if obj := p.TypesInfo.Defs[d.Name]; obj != nil {
						byObj[obj] = x
					}
					if d.Recv != nil {
						methodsByName[d.Name.Name] = append(methodsByName[d.Name.Name], x)
					}
				case *ast.FuncLit:
					all = append(all, &fn{pkg: p, body: d.Body, node: d})
				}
				return true
			})
		}
	}

	litOf := map[*ast.FuncLit]*fn{}
	for _, x := range all {
		if l, ok := x.node.(*ast.FuncLit); ok {
			litOf[l] = x
		}
	}
	var regions []region
	addRegion := func(p *packages.Package, from, to token.Pos, reason string) {
		a, b := p.Fset.Position(from), p.Fset.Position(to)
		regions = append(regions, region{File: a.Filename, Start: a.Line, End: b.Line, Reason: reason})
	}

	// refs[f] records, per function, whether it has a reference outside a condition (true) or only inside one
	// (false); a function absent from it has no reference at all.
	refs := map[ast.Node]bool{}
	ref := func(x *fn, cond bool) {
		if x != nil {
			refs[x.node] = refs[x.node] || !cond
		}
	}
	use := func(p *packages.Package, n ast.Node, cond bool) {
		switch e := n.(type) {
		case *ast.Ident:
			if obj, ok := p.TypesInfo.Uses[e].(*types.Func); ok {
				ref(byObj[obj.Origin()], cond)
			}
		case *ast.SelectorExpr:
			if sel := p.TypesInfo.Selections[e]; sel != nil {
				if m, ok := sel.Obj().(*types.Func); ok && types.IsInterface(sel.Recv()) {
					for _, x := range methodsByName[m.Name()] {
						ref(x, cond)
					}
				}
			}
		case *ast.FuncLit:
			ref(litOf[e], cond)
		}
	}
	// walk reads one function's body (or a package-level declaration), where cond is whether a node lies inside a
	// condition of that function: conditions become regions, references are recorded with it. A nested literal is a
	// function of its own, walked separately, and referenced where it is written.
	var walk func(p *packages.Package, root ast.Node, cond bool)
	walk = func(p *packages.Package, root ast.Node, cond bool) {
		ast.Inspect(root, func(n ast.Node) bool {
			if n == nil {
				return false
			}
			switch s := n.(type) {
			case *ast.FuncLit:
				if n != root {
					use(p, s, cond)
					return false
				}
			case *ast.IfStmt:
				if s.Init != nil {
					walk(p, s.Init, cond)
				}
				walk(p, s.Cond, cond)
				if !cond {
					addRegion(p, s.Body.Pos(), s.Body.End(), "condition")
				}
				walk(p, s.Body, true)
				if s.Else != nil {
					if !cond {
						addRegion(p, s.Else.Pos(), s.Else.End(), "condition")
					}
					walk(p, s.Else, true)
				}
				return false
			case *ast.CaseClause:
				for _, e := range s.List {
					walk(p, e, cond)
				}
				if !cond {
					addRegion(p, s.Pos(), s.End(), "condition")
				}
				for _, st := range s.Body {
					walk(p, st, true)
				}
				return false
			case *ast.CommClause:
				if !cond {
					addRegion(p, s.Pos(), s.End(), "condition")
				}
				for _, st := range s.Body {
					walk(p, st, true)
				}
				return false
			case *ast.Ident, *ast.SelectorExpr:
				use(p, n, cond)
			}
			return true
		})
	}
	for _, x := range all {
		walk(x.pkg, x.body, false)
	}
	for _, p := range mine {
		for _, f := range p.Syntax {
			for _, decl := range f.Decls {
				if g, ok := decl.(*ast.GenDecl); ok && g.Tok == token.VAR {
					walk(p, g, false)
				}
			}
		}
	}
	// A function every reference to which is inside a condition, or which nothing references (the entry point, the
	// harness test and inits aside), is configuration-dependent as a whole.
	for _, x := range all {
		if d, ok := x.node.(*ast.FuncDecl); ok && d.Recv == nil &&
			(d.Name.Name == "init" || (!*test && d.Name.Name == "main" && x.pkg.Name == "main") || (*test && d.Name.Name == "TestKCRoutes")) {
			continue
		}
		unconditional, referenced := refs[x.node]
		switch {
		case !referenced:
			addRegion(x.pkg, x.body.Pos(), x.body.End(), "unreferenced")
		case !unconditional:
			addRegion(x.pkg, x.body.Pos(), x.body.End(), "called under a condition")
		}
	}

	sort.Slice(regions, func(i, j int) bool {
		a, b := regions[i], regions[j]
		if a.File != b.File {
			return a.File < b.File
		}
		if a.Start != b.Start {
			return a.Start < b.Start
		}
		if a.End != b.End {
			return a.End < b.End
		}
		return a.Reason < b.Reason
	})
	out := json.NewEncoder(os.Stdout)
	for _, r := range regions {
		out.Encode(r)
	}
}
