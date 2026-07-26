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

# 1. (Re)build web.js + web.wasm + hexel-tile-history.bin into client/web/.
./build-web.sh

# 2. Stage the single-pane page + build outputs, index.html at the root.
# hexel-tile-history.bin used to travel as web.data, the --preload-file
# package. It is now a plain sibling file that game.html's hexelEnsureHistory
# fetches only when a replay is opened, so main() no longer waits on it — but
# it still has to be IN the zip, because itch serves the unzipped folder as
# static files and the fetch would otherwise 404 the moment someone presses
# Export. The .zst/.gz siblings are deliberately left out: itch does its own
# content negotiation and they would only eat into the 64 MB budget.
# title-map.png/-view.txt are deliberately NOT shipped — game.html's fetch of
# them falls back to the bake compiled into the wasm, which for a static zip
# is the same image.
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp client/web/game.html "$STAGE/index.html"
cp client/web/web.js    "$STAGE/web.js"
cp client/web/web.wasm  "$STAGE/web.wasm"
cp client/web/hexel-tile-history.bin "$STAGE/hexel-tile-history.bin"
cp client/web/favicon.ico "$STAGE/favicon.ico"

# 3. Zip with index.html at the archive root. Deflate is what the 64 MB check
# below is really measuring: the wasm dominates the archive, and the history's
# fixed-width records compress to a fraction of their size — it was the
# history that made this a close call back when it shipped 258 MiB of v3
# records.
rm -f "$OUT"
( cd "$STAGE" && zip -q -r "$OUT" index.html web.js web.wasm hexel-tile-history.bin favicon.ico )

# 3b. Every sibling web.js loads by name must be in the archive; itch serves
# the unzipped folder as static files, so a missing one is a 404 at boot.
for required in $(grep -oE '"[A-Za-z0-9_.-]+\.(wasm|data)"' "$STAGE/web.js" | tr -d '"' | sort -u); do
  unzip -l "$OUT" | grep -qE "[[:space:]]$required\$" \
    || { echo "ERROR: web.js loads '$required' but it is not in the zip." >&2; exit 1; }
done
# The history isn't referenced from web.js any more (game.html fetches it by
# name), so the loop above cannot catch a missing one — check it explicitly.
unzip -l "$OUT" | grep -qE '[[:space:]]hexel-tile-history\.bin$' \
  || { echo "ERROR: hexel-tile-history.bin is not in the zip; replay/export would 404." >&2; exit 1; }

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
