#!/bin/sh
# Records each route's registration site for routes-v1.1 (eval/routes-v1/README.md), inside run.sh's container:
#
#     sites.sh FRAMEWORK MODULE
#
# Copies the framework's module (the version the program's build uses, `go list -m`) out of the module cache, places a
# call to KCRecord where it adds a route (gin's RouterGroup.handle, echo v4's Echo.add, echo v5's DefaultRouter.Add, chi's Mux.handle) and a
# kc_sites.go holding KCRecord and KCSites beside it, and points the container's copy of the program's module at the
# copy with a `replace` (go build -overlay can't replace module cache files). The site is the first stack frame outside
# the framework's module and the standard library. The module cache and the clone are untouched.
# A hook that doesn't land exactly once fails, so a framework version the labeller doesn't know never lists routes
# without their sites.
set -eu
fw=$1 module=$2
src=$(go list -m -f '{{.Dir}}' "$module")
version=$(go list -m -f '{{.Version}}' "$module")
dir=/work/ov/sites/$fw@$version
case $fw in
  gin) pkg=gin file=routergroup.go
       expr='s/(func \(group \*RouterGroup\) handle\(httpMethod, relativePath string, handlers HandlersChain\) IRoutes \{\n\tabsolutePath := group\.calculateAbsolutePath\(relativePath\)\n)/$1\tKCRecord(httpMethod+" "+absolutePath)\n/' ;;
  echo) pkg=echo file=echo.go
       expr='s/(func \(e \*Echo\) add\(host, method, path string, handler HandlerFunc, [^\n]*\) \*Route \{\n)/$1\tKCRecord(method+" "+path)\n/' ;;
  echo5) pkg=echo file=router.go
       expr='s/(func \(r \*DefaultRouter\) Add\(route Route\) \(RouteInfo, error\) \{\n)/$1\tKCRecord(route.Method+" "+route.Path)\n/' ;;
  chi) pkg=chi file=mux.go
       expr='s/(\n)(\treturn mx\.tree\.InsertRoute\(method, pattern, h\)\n)/$1\tKCRecord(KCHandlerKey(handler)+" "+pattern)\n$2/' ;;
  *) echo "sites.sh: no site hook for $fw" >&2; exit 1 ;;
esac
if [ ! -d "$dir" ]; then
  mkdir -p /work/ov/sites && cp -R "$src" "$dir" && chmod -R u+w "$dir"
  # A pre-module version (chi v4 is `+incompatible`) has no go.mod, which a replacement directory needs.
  [ -f "$dir/go.mod" ] || printf 'module %s\n' "$module" > "$dir/go.mod"
  perl -0pi -e "$expr" "$dir/$file"
  [ "$(grep -c 'KCRecord(' "$dir/$file")" = 1 ] || { echo "sites.sh: $fw $version: site hook not placed" >&2; exit 1; }
  cat > "$dir/kc_sites.go" <<GO
package $pkg

import (
	"fmt"
	"net/http"
	"reflect"
	"runtime"
	"strings"
)

// KCSites holds, by route (gin, echo: "METHOD path"; chi: the endpoint handler's identity and the pattern as its own
// router has it), each site that registered it.
var KCSites = map[string][]string{}

var _ = http.MethodGet

// KCRecord keeps the first caller outside this framework's module and the standard library.
func KCRecord(key string) {
	pcs := make([]uintptr, 64)
	frames := runtime.CallersFrames(pcs[:runtime.Callers(2, pcs)])
	for {
		f, more := frames.Next()
		if f.File != "" && !strings.HasPrefix(f.File, runtime.GOROOT()+"/") && !strings.HasPrefix(f.File, "$dir/") {
			KCSites[key] = append(KCSites[key], fmt.Sprintf("%s:%d", f.File, f.Line))
			return
		}
		if !more {
			return
		}
	}
}

// KCHandlerKey names an endpoint handler by its identity, as chi.Walk hands it back.
func KCHandlerKey(h interface{}) string {
	v := reflect.ValueOf(h)
	switch v.Kind() {
	case reflect.Func, reflect.Ptr, reflect.Map, reflect.Chan, reflect.UnsafePointer:
		return fmt.Sprintf("%s@%x", v.Type(), v.Pointer())
	}
	return fmt.Sprintf("%s@%v", v.Type(), h)
}
GO
fi
go mod edit -replace "$module=$dir"
