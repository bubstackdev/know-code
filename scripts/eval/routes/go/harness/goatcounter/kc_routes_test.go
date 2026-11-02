package handlers

// routes-v1.2's held-out harness for arp242/goatcounter (eval/routes-v1/README.md, "Held-out repositories"): builds
// the router `goatcounter serve` serves for any host, NewBackend, with the program's defaults (no ACME handler, not
// development mode, not goatcounter.com, no static domain or base path, a 60-second dashboard timeout, the default API
// limit and rate limits) over the package's own test database, and hands it to the labeller's hook, which lists it.

import (
	"net/http"
	"testing"

	"zgo.at/goatcounter/v2/gctest"
	"zgo.at/zdb"
)

func TestKCRoutes(t *testing.T) {
	ctx := gctest.DB(t)
	http.KCServeHook(NewBackend(zdb.MustGetDB(ctx), nil, false, false, "", "", 60, 0, NewRatelimits()))
}
