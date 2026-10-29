#!/bin/sh
# Prepares the route labeller's overlay (eval/routes-v1/README.md, Go), inside run.sh's container: a copy of the
# toolchain's net/http server.go whose Serve, ListenAndServe and ListenAndServeTLS hand their handler to KCServeHook
# instead of serving, and whose ServeMux keeps each pattern it registers, with the registration site it records, in
# KCRegistrations (kept by net/http itself, so a package registering in its own init is seen too). The toolchain itself isn't changed: `go build -overlay` reads the copy.
set -eu
G=$(go env GOROOT)/src/net/http
mkdir -p /work/ov/nethttp
perl -0pe '
  s/(func \((\w+) \*Server\) Serve\(l net\.Listener\) error \{\n)/$1\tif KCServeHook != nil { KCServeHook($2.Handler) }\n/;
  s/(func \((\w+) \*Server\) ListenAndServe\(\) error \{\n)/$1\tif KCServeHook != nil { KCServeHook($2.Handler) }\n/;
  s/(func \((\w+) \*Server\) ListenAndServeTLS\(certFile, keyFile string\) error \{\n)/$1\tif KCServeHook != nil { KCServeHook($2.Handler) }\n/;
  s/(\tmux\.tree\.addPattern\(pat, handler\)\n)/$1\tKCRegistrations = append(KCRegistrations, KCRegistration{mux, patstr, pat.loc, handler})\n/;
' $G/server.go > /work/ov/nethttp/server.go
# Each hook must land exactly where the labeller expects, or the toolchain's net/http has changed under it.
[ "$(grep -c 'if KCServeHook != nil' /work/ov/nethttp/server.go)" = 3 ] || { echo "patch.sh: serve hooks not placed" >&2; exit 1; }
[ "$(grep -c 'KCRegistrations = append' /work/ov/nethttp/server.go)" = 1 ] || { echo "patch.sh: register hook not placed" >&2; exit 1; }
cat > /work/ov/nethttp/kc_hook.go <<'GO'
package http

// KCServeHook and KCRegistrations are the route labeller's: a serve call hands its handler to KCServeHook instead of
// serving, and each pattern a ServeMux registers is kept in KCRegistrations with where it was registered.
var KCServeHook func(Handler)

type KCRegistration struct {
	Mux              *ServeMux
	Pattern, Loc     string
	Handler          Handler
}

var KCRegistrations []KCRegistration
GO
printf '"%s/server.go": "/work/ov/nethttp/server.go",\n"%s/kc_hook.go": "/work/ov/nethttp/kc_hook.go"' $G $G > /work/ov/std-entries
