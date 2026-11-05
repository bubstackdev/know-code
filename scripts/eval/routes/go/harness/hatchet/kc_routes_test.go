package run

// routes-v1.2's round-3 held-out harness for hatchet-dev/hatchet (eval/routes-v1/README.md, "Held-out repositories,
// round 3"): builds the echo instance the API server serves, as Run does through getCoreEchoService, over a server
// configuration short of the database, message queue and other services the program connects to before it builds
// it, and hands it to the labeller's hook, which lists its routes. Registering the routes calls none of the handlers.

import (
	"net/http"
	"testing"

	"github.com/rs/zerolog"

	"github.com/hatchet-dev/hatchet/pkg/config/server"
)

func TestKCRoutes(t *testing.T) {
	logger := zerolog.Nop()
	e, err := NewAPIServer(&server.ServerConfig{Logger: &logger}).getCoreEchoService()
	if err != nil {
		t.Fatal(err)
	}
	http.KCServeHook(e)
}
