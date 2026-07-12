#!/usr/bin/env bash
# Builds the web client (client/src/bin/web.rs) to wasm32-unknown-emscripten
# and stages it next to client/web/index.html, ready to serve.
set -e

EMSDK_DIR="${EMSDK_DIR:-./emsdk}"
# shellcheck disable=SC1091
. "$EMSDK_DIR/emsdk_env.sh"

# -sINITIAL_MEMORY=64MB (default is 16MB): the sound rework embeds ~2.8MB
# of audio and decodes several MP3s to raw PCM at startup (`Sfx::load`'s
# `new_wave_from_memory` calls, unlike `Music` which streams) — that blew
# past the 16MB default and crashed the tab. Deliberately NOT
# -sALLOW_MEMORY_GROWTH=1: growable wasm memory backs its buffer with a
# resizable ArrayBuffer, which some browsers' TextDecoder.decode() rejects
# outright ("The provided ArrayBuffer value must not be resizable"),
# breaking every embind/UTF8 string call. A larger fixed size sidesteps
# both problems.
export EMCC_CFLAGS="-O3 -sUSE_GLFW=3 -sASSERTIONS=1 -sWASM=1 -sASYNCIFY -sGL_ENABLE_GET_PROC_ADDRESS=1 -sINITIAL_MEMORY=67108864"
if [ -x "$EMSDK_DIR/upstream/emscripten/emcc" ]; then
    source "$EMSDK_DIR/emsdk_env.sh"
fi
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(em-config CACHE)/sysroot/include"

# Debug builds crash binaryen's wasm-emscripten-finalize (DWARF info clashes
# with ASYNCIFY + wasm exception handling) — always build release.
cargo build -p client --bin web --target wasm32-unknown-emscripten --release

cp target/wasm32-unknown-emscripten/release/web.js client/web/web.js
cp target/wasm32-unknown-emscripten/release/web.wasm client/web/web.wasm

echo "Built client/web/{index.html,web.js,web.wasm}"
echo "Serve it, e.g.: python3 -m http.server -d client/web 8080"
