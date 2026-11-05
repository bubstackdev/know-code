#!/bin/sh
# routes-v1.2's round-3 held-out navidrome scans its music folder (`./music` by default) when it starts: an empty one in
# the container's copy lets it start. The routes it registers don't depend on the folder's contents.
mkdir -p music
