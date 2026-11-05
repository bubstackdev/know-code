#!/bin/sh
# routes-v1.2's round-3 held-out gokapi exits when its database has no user. Its non-interactive deployment run
# (`--deployment-password`) creates the super-admin and exits without serving; the listed run then starts normally.
cd cmd/gokapi && go run . --deployment-password kc-routes-label
