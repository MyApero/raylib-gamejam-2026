#!/bin/sh
# Builds the web client (client/src/bin/web.rs) to wasm32-unknown-emscripten
# and stages it next to client/web/index.html, ready to serve.
set -e

EMSDK_DIR="${EMSDK_DIR:-../emsdk}"
# shellcheck disable=SC1091
. "$EMSDK_DIR/emsdk_env.sh"

export EMCC_CFLAGS="-O3 -sUSE_GLFW=3 -sASSERTIONS=1 -sWASM=1 -sASYNCIFY -sGL_ENABLE_GET_PROC_ADDRESS=1"
export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(cd "$EMSDK_DIR" && pwd)/upstream/emscripten/cache/sysroot/include"

# Debug builds crash binaryen's wasm-emscripten-finalize (DWARF info clashes
# with ASYNCIFY + wasm exception handling) — always build release.
cargo build -p client --bin web --target wasm32-unknown-emscripten --release

cp target/wasm32-unknown-emscripten/release/web.js client/web/web.js
cp target/wasm32-unknown-emscripten/release/web.wasm client/web/web.wasm

echo "Built client/web/{index.html,web.js,web.wasm}"
echo "Serve it, e.g.: python3 -m http.server -d client/web 8080"
