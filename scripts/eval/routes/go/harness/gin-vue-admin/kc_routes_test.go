package core

// routes-v1.2's round-2 held-out harness for flipped-aurora/gin-vue-admin (eval/routes-v1/README.md, "Held-out
// repositories, round 2"): initialises the server as its main does short of the database (the configuration from the
// repository's config.yaml, the other settings, the logger and the global handlers), builds the gin engine RunServer
// serves, initialize.Routers(), and hands it to the labeller's hook, which lists its routes. Without the configuration
// Routers() registers its file store at an empty path, whose catch-all conflicts with the swagger route.

import (
	"net/http"
	"os"
	"testing"

	"github.com/flipped-aurora/gin-vue-admin/server/global"
	"github.com/flipped-aurora/gin-vue-admin/server/initialize"
	"go.uber.org/zap"
)

func TestKCRoutes(t *testing.T) {
	os.Setenv("GVA_CONFIG", "../config.yaml")
	global.GVA_VP = Viper()
	initialize.OtherInit()
	global.GVA_LOG = Zap()
	zap.ReplaceGlobals(global.GVA_LOG)
	initialize.SetupHandlers()
	http.KCServeHook(initialize.Routers())
}
