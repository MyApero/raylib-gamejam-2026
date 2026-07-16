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
# past the 16MB default and crashed the tab. The recovered tile replay is a
# 145MB preloaded binary stream, read sequentially by `web.rs`, so it needs
# the larger fixed heap as well. Deliberately NOT
# -sALLOW_MEMORY_GROWTH=1: growable wasm memory backs its buffer with a
# resizable ArrayBuffer, which some browsers' TextDecoder.decode() rejects
# outright ("The provided ArrayBuffer value must not be resizable"),
# breaking every embind/UTF8 string call. A larger fixed size sidesteps
# both problems.
HISTORY_SOURCE="tools/history-extractor/output/hexel-tile-history-v3.bin"
HISTORY_WEB="client/web/hexel-tile-history.bin"
if [ ! -f "$HISTORY_SOURCE" ]; then
    echo "Missing recovered history: $HISTORY_SOURCE" >&2
    echo "Run tools/history-extractor first; see tools/history-extractor/README.md" >&2
    exit 1
fi
cp "$HISTORY_SOURCE" "$HISTORY_WEB"

# Export HEAPU8 as well: the GIF encoder hands its completed byte buffer to
# browser JavaScript, which must copy those bytes before Rust frees it.
export EMCC_CFLAGS="-O3 -sUSE_GLFW=3 -sASSERTIONS=1 -sWASM=1 -sASYNCIFY -sGL_ENABLE_GET_PROC_ADDRESS=1 -sINITIAL_MEMORY=268435456 -sEXPORTED_RUNTIME_METHODS=HEAPU8 --preload-file $HISTORY_WEB@/hexel-tile-history.bin"
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
cp target/wasm32-unknown-emscripten/release/deps/web.data client/web/web.data

echo "Built client/web/{index.html,web.js,web.wasm,web.data}"
echo "Serve it, e.g.: python3 -m http.server -d client/web 8080"
