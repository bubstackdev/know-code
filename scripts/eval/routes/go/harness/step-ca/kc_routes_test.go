package api

// routes-v1.2's round-2 held-out harness for smallstep/certificates (eval/routes-v1/README.md, "Held-out repositories,
// round 2"): builds the API's routes on a chi router as ca/ca.go does (chi.NewRouter, then Route) and hands the router
// to the labeller's hook, which lists its routes.

import (
	"net/http"
	"testing"

	"github.com/go-chi/chi/v5"
)

func TestKCRoutes(t *testing.T) {
	mux := chi.NewRouter()
	Route(mux)
	http.KCServeHook(mux)
}
