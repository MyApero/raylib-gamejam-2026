#!/usr/bin/env bash
#
# Re-bake the title-screen map whenever the island layout changes.
#
# Islands change slot on every leaderboard re-rank (RERANK_PERIOD_SECS = 300),
# and the title backdrop is a picture of where they were, so a bake goes stale
# within minutes of being made. This keys off the layout itself rather than a
# clock: the set of (island id, slot) pairs is exactly what the picture
# depends on, and it also catches islands being created or reaped, which move
# things too. Painting does not trigger a re-bake — cells change constantly
# and the daily refresh picks those up.
#
# Renders into client/assets (the compiled-in fallback, picked up by the next
# build) AND client/web (served next to the page, fetched at runtime), so the
# two never drift apart. Players see the new backdrop on their next load, with
# no rebuild and no cache-bust needed — see `hexelLoadTitleMap` in game.html.
#
# Run from a systemd user timer every minute; safe to run by hand.

set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
DB_NAME="${HEXEL_DB:-hexel}"
STATE="$REPO_ROOT/tools/map-image/.island-layout"

layout=$(spacetime sql -s local "$DB_NAME" "SELECT id, slot FROM island" 2>/dev/null \
    | sha256sum | cut -d' ' -f1)
# An empty/failed query hashes to a constant, which would look like a change
# and then like a permanent state. Bail instead: the server is down or the
# database is unreachable, and the previous bake is still the best one we have.
if [ -z "$layout" ] || ! spacetime sql -s local "$DB_NAME" \
        "SELECT id FROM island" >/dev/null 2>&1; then
    exit 0
fi

if [ "$layout" = "$(cat "$STATE" 2>/dev/null || true)" ]; then
    exit 0
fi

cd "$REPO_ROOT/tools/map-image"
./render_map.py "$DB_NAME" ../../client/assets/title-map >/dev/null
for asset in title-map.png title-map-view.txt; do
    cp "../../client/assets/$asset" "../../client/web/$asset"
done
printf '%s' "$layout" > "$STATE"
echo "[hexel-map] island layout changed; title map re-baked"
