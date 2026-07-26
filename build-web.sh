#!/usr/bin/env bash
# Builds the web client (client/src/bin/web.rs) to wasm32-unknown-emscripten
# and stages it next to client/web/index.html, ready to serve.
set -e

EMSDK_DIR="${EMSDK_DIR:-./emsdk}"
# shellcheck disable=SC1091
. "$EMSDK_DIR/emsdk_env.sh"

# -sINITIAL_MEMORY=256MB (default is 16MB): the sound rework embeds ~2.8MB
# of audio and decodes several MP3s to raw PCM at startup (`Sfx::load`'s
# `new_wave_from_memory` calls, unlike `Music` which streams) — that blew
# past the 16MB default and crashed the tab. Deliberately NOT
# -sALLOW_MEMORY_GROWTH=1: growable wasm memory backs its buffer with a
# resizable ArrayBuffer, which some browsers' TextDecoder.decode() rejects
# outright ("The provided ArrayBuffer value must not be resizable"),
# breaking every embind/UTF8 string call. A larger fixed size sidesteps
# both problems.
# The recovered tile replay. Served as a plain sibling file and fetched at
# runtime by `hexelEnsureHistory` in game.html, NOT baked into the bundle:
# it used to be an emscripten `--preload-file`, which registers it as a run
# dependency, and emscripten will not call main() until every run dependency
# resolves. That put a 258 MiB download in front of the title screen for
# every visitor — including the overwhelming majority who never open a
# replay at all. Nothing reads it before `RecoveredReplay::open`, so it has
# no business gating startup.
HISTORY_SOURCE="tools/history-extractor/output/hexel-tile-history-v3.bin"
HISTORY_WEB="client/web/hexel-tile-history.bin"
if [ ! -f "$HISTORY_SOURCE" ]; then
    echo "Missing recovered history: $HISTORY_SOURCE" >&2
    echo "Run tools/history-extractor first; see tools/history-extractor/README.md" >&2
    exit 1
fi
cp "$HISTORY_SOURCE" "$HISTORY_WEB"

# Precompressed siblings for Caddy's `file_server { precompressed }`. These
# 17-byte fixed records are enormously redundant, so zstd -19 takes 258 MiB
# down to ~15 MiB on the wire. Compressing at build time rather than via
# Caddy's `encode` matters: `encode` would re-compress the whole 258 MiB on
# every cache miss, burning VPS CPU to produce identical output each time.
# Only rebuilt when the source is newer, since zstd -19 is slow.
for ext in zst gz; do
    if [ ! -f "$HISTORY_WEB.$ext" ] || [ "$HISTORY_SOURCE" -nt "$HISTORY_WEB.$ext" ]; then
        echo "Compressing history -> $HISTORY_WEB.$ext (slow, cached until the history changes)"
        case "$ext" in
            zst) zstd -19 -T0 -q -f -o "$HISTORY_WEB.$ext" "$HISTORY_WEB" ;;
            gz)  gzip -9 -c "$HISTORY_WEB" > "$HISTORY_WEB.$ext" ;;
        esac
    fi
done

# Served alongside the page, NOT preloaded: the wasm carries its own copy of
# this bake for the first frame, then fetches these at runtime and swaps to
# them. That is what lets a re-rank refresh the title backdrop without
# rebuilding the bundle — see `hexelLoadTitleMap` in game.html. Seeded from
# the compiled-in pair so a fresh checkout serves something.
for asset in title-map.png title-map-view.txt; do
    cp "client/assets/$asset" "client/web/$asset"
done

# Export HEAPU8 as well: the GIF encoder hands its completed byte buffer to
# browser JavaScript, which must copy those bytes before Rust frees it.
# FS/writeFile are exported so hexelEnsureHistory can drop the fetched
# history into MEMFS under the path `RecoveredReplay` opens; without
# --preload-file emscripten no longer pulls the FS bindings in on its own.
export EMCC_CFLAGS="-O3 -sUSE_GLFW=3 -sASSERTIONS=1 -sWASM=1 -sASYNCIFY -sGL_ENABLE_GET_PROC_ADDRESS=1 -sINITIAL_MEMORY=268435456 -sEXPORTED_RUNTIME_METHODS=HEAPU8,FS -sFORCE_FILESYSTEM=1"
if [ -x "$EMSDK_DIR/upstream/emscripten/emcc" ]; then
    source "$EMSDK_DIR/emsdk_env.sh"
fi
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(em-config CACHE)/sysroot/include"

# A small bridge around raylib's bundled `msf_gif` encoder makes GIF export
# self-contained in the browser build; the Rust web client links this object
# through Emscripten alongside raylib.
GIF_OBJECT="target/wasm32-unknown-emscripten/hexel-gif-export.o"
mkdir -p "$(dirname "$GIF_OBJECT")"
emcc -O3 -c client/src/gif_export.c -o "$GIF_OBJECT"
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=$(pwd)/$GIF_OBJECT"

# Debug builds crash binaryen's wasm-emscripten-finalize (DWARF info clashes
# with ASYNCIFY + wasm exception handling) — always build release.
cargo build -p client --bin web --target wasm32-unknown-emscripten --release

cp target/wasm32-unknown-emscripten/release/web.js client/web/web.js
cp target/wasm32-unknown-emscripten/release/web.wasm client/web/web.wasm
# No web.data any more: with --preload-file gone emscripten emits no data
# package at all, and the history is served as its own file instead.
rm -f client/web/web.data

# wasm-opt is emsdk's own binaryen build, so no separate install is needed —
# emcc skips it only because the bare `wasm-opt` isn't on PATH. -O3 plus
# ASYNCIFY-aware passes; --enable-* must mirror the features emcc emitted or
# the module fails to validate.
WASM_OPT="$EMSDK_DIR/upstream/bin/wasm-opt"
if [ -x "$WASM_OPT" ]; then
    before=$(stat -c%s client/web/web.wasm)
    "$WASM_OPT" -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
        --enable-exception-handling --enable-sign-ext --enable-mutable-globals \
        client/web/web.wasm -o client/web/web.wasm.opt \
        && mv client/web/web.wasm.opt client/web/web.wasm \
        || { echo "wasm-opt failed; keeping the unoptimised module" >&2; rm -f client/web/web.wasm.opt; }
    after=$(stat -c%s client/web/web.wasm)
    printf 'wasm-opt: %s -> %s bytes\n' "$before" "$after"
else
    echo "wasm-opt not found at $WASM_OPT; shipping the unoptimised module" >&2
fi

echo "Built client/web/{index.html,web.js,web.wasm} + hexel-tile-history.bin{,.zst,.gz}"
echo "Serve it, e.g.: python3 -m http.server -d client/web 8080"
