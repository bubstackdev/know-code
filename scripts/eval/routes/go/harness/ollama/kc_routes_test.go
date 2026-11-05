package server

// routes-v1.2's round-3 held-out harness for ollama/ollama (eval/routes-v1/README.md, "Held-out repositories, round
// 3"): `ollama serve` serves http.DefaultServeMux with the gin engine GenerateRoutes builds mounted at `/`, which the
// dump lists as one net/http pattern. This builds that engine as the package's own tests do (codex_proxy_test.go: a
// Server on the default port) and hands it to the labeller's hook, which lists its routes.

import (
	"net"
	"net/http"
	"testing"
)

func TestKCRoutes(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	h, err := (&Server{addr: &net.TCPAddr{IP: net.IPv4zero, Port: 11434}}).GenerateRoutes()
	if err != nil {
		t.Fatal(err)
	}
	http.KCServeHook(h)
}
