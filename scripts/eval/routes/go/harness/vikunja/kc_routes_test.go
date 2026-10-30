package routes

// routes-v1.2's round-2 held-out harness for go-vikunja/vikunja (eval/routes-v1/README.md, "Held-out repositories,
// round 2"): builds the echo instance `vikunja web` serves, NewEcho then RegisterRoutes, after the setup the
// repository's own route tests make short of the database (pkg/caldavtests: the default configuration, the logger,
// fake events and the key-value store), and hands it to the labeller's hook, which lists its routes.

import (
	"net/http"
	"testing"

	"code.vikunja.io/api/pkg/config"
	"code.vikunja.io/api/pkg/events"
	"code.vikunja.io/api/pkg/log"
	"code.vikunja.io/api/pkg/modules/keyvalue"
)

func TestKCRoutes(t *testing.T) {
	config.InitDefaultConfig()
	config.ServicePublicURL.Set("https://localhost")
	log.InitLogger()
	events.Fake()
	keyvalue.InitStorage()
	e := NewEcho()
	RegisterRoutes(e)
	http.KCServeHook(e)
}
