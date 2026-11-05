package api

// The route labeller's harness for urgentry (scripts/eval/routes/go/list.sh, a `test:` program): builds the API's
// routes on a ServeMux as the package's own tests build its router (sqliteAuthorizedDependencies over a test
// database) and hands the mux to the labeller's hook, which lists what the mux registered.

import (
	"net/http"
	"testing"
)

func TestKCRoutes(t *testing.T) {
	db := openTestSQLite(t)
	mux := http.NewServeMux()
	if err := RegisterRoutesInto(mux, sqliteAuthorizedDependencies(t, db, Dependencies{})); err != nil {
		t.Fatal(err)
	}
	http.KCServeHook(mux)
}
