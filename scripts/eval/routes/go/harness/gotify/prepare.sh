#!/bin/sh
# gotify embeds its web UI's build output (ui/build), which isn't committed, and reads index.html and manifest.json
# from it when it registers the UI's routes: empty stand-ins let the server build and start. Which routes the Go code
# registers doesn't depend on what the files hold.
set -eu
mkdir -p ui/build
: > ui/build/index.html
echo '{}' > ui/build/manifest.json
