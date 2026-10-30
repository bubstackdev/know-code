package routes

// routes-v1.2's round-2 held-out harness for go-vikunja/vikunja (eval/routes-v1/README.md, "Held-out repositories,
// round 2"): builds the echo instance `vikunja web` serves, NewEcho then RegisterRoutes, over the default
// configuration as the package's tests set it, and hands it to the labeller's hook, which lists its routes.

import (
	"net/http"
	"testing"

	"code.vikunja.io/api/pkg/config"
)

func TestKCRoutes(t *testing.T) {
	config.InitDefaultConfig()
	e := NewEcho()
	RegisterRoutes(e)
	http.KCServeHook(e)
}
