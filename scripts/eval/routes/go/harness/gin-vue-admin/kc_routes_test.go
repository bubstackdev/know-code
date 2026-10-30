package initialize

// routes-v1.2's round-2 held-out harness for flipped-aurora/gin-vue-admin (eval/routes-v1/README.md, "Held-out
// repositories, round 2"): builds the gin engine the server serves, Routers(), with the default configuration, and
// hands it to the labeller's hook, which lists its routes.

import (
	"net/http"
	"testing"
)

func TestKCRoutes(t *testing.T) {
	http.KCServeHook(Routers())
}
