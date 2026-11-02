#!/bin/sh
# Lists the routes of Go programs as their framework lists them (eval/routes-v1/README.md, Go), inside run.sh's
# container, offline.
#
#     list.sh REPO MODDIR PKG [ARG ...]
#
# Builds the main package PKG of the module at MODDIR with the overlay patch.sh prepared: when the program would start
# serving, the dump (dump.go.tmpl) prints its router's own listing and exits. ARGs are passed to the program. A PKG of
# `test:DIR` instead runs the overlay test DIR/kc_routes_test.go from the labeller's harness directory, for a router
# a library builds (a test there builds it as the repository's own tests do, and serves it to the hook).
#
# Prints JSON lines: one `program` row (the package, the repository directories it compiles, and `listed`, or why
# not), then a `route` row per route.
set -u
repo=$1 mod=$2 pkg=$3; shift 3
cd "/work/src/$mod"

imports="" cases=""
add() { imports="$imports
	$1"; cases="$cases
$2"; }
# routes-v1.1 (run.sh SITES=1): each route's registration site, from a hook in the framework (sites.sh).
site() { if [ -f /in/sites ]; then sh /work/tools/sites.sh "$1" "$2" || { printf '{"t": "program", "repo": "%s", "module": "%s", "package": "%s", "status": "site hook", "dirs": ""}\n' "$repo" "$mod" "$pkg"; exit 0; }; fi; }
if grep -q 'github.com/gin-gonic/gin ' go.mod; then
  add '"github.com/gin-gonic/gin"' '		case *gin.Engine:
			for _, rt := range r.Routes() {
				kcOutAt(kcRoute{Framework: "gin", Method: rt.Method, Path: rt.Path, Handler: rt.Handler}, gin.KCSites, rt.Method+" "+rt.Path)
			}'
  site gin github.com/gin-gonic/gin
fi
# echo v4 is imported as echo4, so that a module requiring both v4 and v5 can list both. A module requiring both lists
# v4 only when the program builds it (it may be another dependency's).
echo4_built() {
  grep -q 'github.com/labstack/echo/v5 ' go.mod || return 0
  case $pkg in test:*) d=${pkg#test:}; t=-test ;; *) d=$pkg; t= ;; esac
  go list -deps $t "./$d" 2>/dev/null | grep -qx 'github.com/labstack/echo/v4'
}
if grep -q 'github.com/labstack/echo/v4 ' go.mod && echo4_built; then
  add 'echo4 "github.com/labstack/echo/v4"' '		case *echo4.Echo:
			for _, rt := range r.Routes() {
				kcOutAt(kcRoute{Framework: "echo", Method: rt.Method, Path: rt.Path, Handler: rt.Name}, echo4.KCSites, rt.Method+" "+rt.Path)
			}'
  site echo github.com/labstack/echo/v4
fi
if grep -q 'github.com/labstack/echo/v5 ' go.mod; then
  add '"github.com/labstack/echo/v5"' '		case *echo.Echo:
			for _, rt := range r.Router().Routes() {
				// echo v5 lists a route answering every method (Any) under its own marker: method *, as the rule writes it.
				m := rt.Method
				if m == echo.RouteAny {
					m = "*"
				}
				kcOutAt(kcRoute{Framework: "echo", Method: m, Path: rt.Path, Handler: rt.Name}, echo.KCSites, rt.Method+" "+rt.Path)
			}'
  site echo5 github.com/labstack/echo/v5
fi
# A framework is in the build when go.mod requires it, or is it (chi's own examples, in its module).
for chi in github.com/go-chi/chi/v5 github.com/go-chi/chi; do
  if grep -qE "(^module $chi\$|[[:space:]]$chi[[:space:]])" go.mod; then
    add "\"$chi\"" '		case chi.Routes:
			chi.Walk(r, func(method, route string, h http.Handler, _ ...func(http.Handler) http.Handler) error {
				name := kcFuncName(h)
				// A Mount of a handler chi cannot walk into is listed once per method at prefix/*: one route, any method.
				if strings.Contains(name, ".(*Mux).Mount.func") {
					if kcMounts[route] {
						return nil
					}
					kcMounts[route] = true
					method = "*"
				}
				kcOutAt(kcRoute{Framework: "chi", Method: method, Path: route, Handler: name}, chi.KCSites, kcChiKey(chi.KCSites, chi.KCHandlerKey(h), route))
				return nil
			})'
    # chi's own repository builds chi from its source: no module to hook, so its routes are listed without sites.
    if grep -qE "^module $chi\$" go.mod; then unhooked=chi; else site chi "$chi"; fi
    break
  fi
done

# Without the site hooks, a route is printed as its framework lists it.
[ -f /in/sites ] || unhooked='\w+'
[ -z "${unhooked:-}" ] || cases=$(printf '%s\n' "$cases" | perl -pe "s/kcOutAt\\((kcRoute\\{.*\\}), $unhooked\\.KCSites, .*\\)\$/kcOut(\$1)/")

case $pkg in
  test:*) dir=${pkg#test:}; target_dir=$(cd "$dir" && pwd); dump_pkg=$(go list -f '{{.Name}}' "./$dir") ;;
  *) dir=$pkg; target_dir=$(cd "$pkg" && pwd); dump_pkg=main ;;
esac
sed "s/^package main$/package $dump_pkg/" /work/tools/dump.go.tmpl \
  | awk -v imports="$imports" -v cases="$cases" '{ if ($0 ~ /\/\/IMPORTS/) print imports; else if ($0 ~ /\/\/CASES/) print cases; else print }' \
  > /work/ov/dump.go
extra=""
case $pkg in
  test:*) extra=",
\"$target_dir/kc_routes_test.go\": \"/work/tools/harness/$repo/kc_routes_test.go\"" ;;
esac
printf '{"Replace": {%s,\n"%s/kc_dump.go": "/work/ov/dump.go"%s}}\n' "$(cat /work/ov/std-entries)" "$target_dir" "$extra" > /work/ov/overlay.json

# The repository directories the program compiles: its routes can only come from these.
dirs=$(go list -deps -f '{{if not .Standard}}{{.Dir}}{{end}}' "./$dir" 2>/dev/null | sed -n "s#^/work/src/##p" | sort -u | tr '\n' ' ')
program() {
  printf '{"t": "program", "repo": "%s", "module": "%s", "package": "%s", "status": "%s", "dirs": "%s"}\n' "$repo" "$mod" "$pkg" "$1" "$dirs"
}
case $pkg in
  test:*)
    timeout 120 go test -overlay /work/ov/overlay.json -count=1 -run '^TestKCRoutes$' "./$dir" >/work/run.out 2>/work/run.err ;;
  *)
    if ! go build -overlay /work/ov/overlay.json -o /work/app "./$pkg" 2>/work/run.err; then
      program build; head -5 /work/run.err >&2; exit 0
    fi
    (cd "$pkg" && timeout 60 /work/app "$@" >/work/run.out 2>/work/run.err) ;;
esac
# An `unknown` row is a served handler the dump can't list: the program lists nothing.
out=$(grep '^{"t": "route"' /work/run.out | grep -v '"framework":"unknown"')
if [ -z "$out" ]; then
  program "no listing"; tail -20 /work/run.out >&2; tail -40 /work/run.err >&2
else
  program listed
  printf '%s\n' "$out" | sed "s#^{\"t\": \"route\"#{\"t\": \"route\", \"repo\": \"$repo\", \"module\": \"$mod\", \"package\": \"$pkg\"#"
fi
