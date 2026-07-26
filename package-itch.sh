#!/usr/bin/env bash
# Builds the web client and packages it as an itch.io HTML5 submission zip.
#
# The zip's root has index.html = the SINGLE-game page (client/web/game.html);
# the dual-iframe client/web/index.html is only a LOCAL two-player test harness
# and must NOT be the submission page. Output: hexel-itch.zip.
#
# itch.io upload settings: "This file will be played in the browser",
# viewport exactly 720x720, enable the fullscreen button, set mobile-friendly.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

STAGE="$ROOT/build/itch"          # under build/, which .gitignore ignores
OUT="$ROOT/hexel-itch.zip"
LIMIT=$((64 * 1024 * 1024))       # itch jam hard limit: 64 MB

# 1. (Re)build web.js + web.wasm + web.data into client/web/.
./build-web.sh

# 2. Stage the single-pane page + build outputs, index.html at the root.
# web.data is the --preload-file package holding the replay history: web.js
# fetches it as a run dependency, so without it main() never runs at all.
# title-map.png/-view.txt are deliberately NOT shipped — game.html's fetch of
# them falls back to the bake compiled into the wasm, which for a static zip
# is the same image.
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp client/web/game.html "$STAGE/index.html"
cp client/web/web.js    "$STAGE/web.js"
cp client/web/web.wasm  "$STAGE/web.wasm"
cp client/web/web.data  "$STAGE/web.data"

# 3. Zip with index.html at the archive root.
rm -f "$OUT"
( cd "$STAGE" && zip -q -r "$OUT" index.html web.js web.wasm web.data )

# 3b. Every sibling web.js loads by name must be in the archive; itch serves
# the unzipped folder as static files, so a missing one is a 404 at boot.
for required in $(grep -oE '"[A-Za-z0-9_.-]+\.(wasm|data)"' "$STAGE/web.js" | tr -d '"' | sort -u); do
  unzip -l "$OUT" | grep -qE "[[:space:]]$required\$" \
    || { echo "ERROR: web.js loads '$required' but it is not in the zip." >&2; exit 1; }
done

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
