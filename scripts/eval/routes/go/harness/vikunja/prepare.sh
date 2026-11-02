#!/bin/sh
# routes-v1.2's round-2 held-out vikunja embeds its front end's build (frontend/dist), which isn't committed: an empty
# stand-in in the container's copy lets the Go code build. The routes the Go code registers don't depend on it.
mkdir -p frontend/dist && printf '<!doctype html>\n' > frontend/dist/index.html
