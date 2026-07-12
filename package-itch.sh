#!/usr/bin/env bash
# Builds the web client and packages it as an itch.io HTML5 submission zip.
#
# The zip's root has index.html = the SINGLE-game page (client/web/game.html);
# the dual-iframe client/web/index.html is only a LOCAL two-player test harness
# and must NOT be the submission page. Output: hexaworld-itch.zip.
#
# itch.io upload settings: "This file will be played in the browser",
# viewport exactly 720x720, enable the fullscreen button, set mobile-friendly.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

STAGE="$ROOT/build/itch"          # under build/, which .gitignore ignores
OUT="$ROOT/hexel-itch.zip"
LIMIT=$((64 * 1024 * 1024))       # itch jam hard limit: 64 MB

# 1. (Re)build web.js + web.wasm into client/web/.
./build-web.sh

# 2. Stage the single-pane page + build outputs, index.html at the root.
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp client/web/game.html "$STAGE/index.html"
cp client/web/web.js    "$STAGE/web.js"
cp client/web/web.wasm  "$STAGE/web.wasm"

# 3. Zip with index.html at the archive root.
rm -f "$OUT"
( cd "$STAGE" && zip -q -r "$OUT" index.html web.js web.wasm )

# 4. Report + enforce the 64 MB limit.
echo
echo "=== $OUT ==="
unzip -l "$OUT"
SIZE=$(stat -c%s "$OUT")
printf 'Total: %s (%d bytes)\n' "$(du -h "$OUT" | cut -f1)" "$SIZE"
if [ "$SIZE" -ge "$LIMIT" ]; then
  echo "ERROR: zip is >= 64 MB, over the jam limit." >&2
  exit 1
fi
echo "OK: under the 64 MB jam limit. Upload as a DRAFT and test end-to-end first."
