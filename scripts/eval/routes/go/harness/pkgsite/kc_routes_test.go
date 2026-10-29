package frontend

// routes-v1.2's held-out harness for golang/pkgsite (eval/routes-v1/README.md, "Held-out repositories"): builds the
// frontend's ServeMux as the package's own tests do (newTestServer: a fake data source and the embedded files), and
// hands it to the labeller's hook, which lists its registrations.

import (
	"net/http"
	"testing"
)

func TestKCRoutes(t *testing.T) {
	_, mux := newTestServer(t, nil)
	http.KCServeHook(mux)
}
