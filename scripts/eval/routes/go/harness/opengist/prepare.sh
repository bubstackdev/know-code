#!/bin/sh
# opengist reads its front-end build's manifest (.vite/manifest.json) when it starts, from the directory it runs in
# (public.Files is os.DirFS(".") without the fs_embed tag), and that build isn't committed: an empty manifest at the
# repository's root, where the program runs, lets it start. Which routes the Go code registers doesn't depend on it.
set -eu
mkdir -p .vite
echo '{}' > .vite/manifest.json
