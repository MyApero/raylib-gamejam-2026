# Running the PoC

## Setup (once per machine)

```bash
curl -sSf https://install.spacetimedb.com | sh
rustup target add wasm32-unknown-unknown
```

## Terminal 1 — SpacetimeDB

```bash
spacetime start
```

Keep this running. First time only (and again any time `server/src/lib.rs` changes):

```bash
./server/publish.sh
./generate_module_bindings.sh
```

## Terminal 2 — client

```bash
cargo run -p client
```

Run this command in extra terminals to spawn more players.

## Terminal 3 — web build (optional)

Requires emsdk activated (`emsdk_env.sh` sourced) — `build-web.sh` does this
for you (assumes `../emsdk`, override with `EMSDK_DIR=...`).

```bash
./build-web.sh
python3 -m http.server -d client/web 8080
```

Then open http://localhost:8080 in a browser. Debug builds crash the
emscripten linker (binaryen assertion), so the script always builds
`--release`.

The web client polls SpacetimeDB's HTTP API instead of holding a WebSocket
(raylib's web target is emscripten; spacetimedb-sdk's browser feature needs
wasm-bindgen, which doesn't support emscripten). See `client/src/bin/web.rs`.

## VPS setup (SpacetimeDB 2.7, for later)

```bash
curl -sSf https://install.spacetimedb.com | sh
spacetime start --listen-addr 0.0.0.0:3000
```

Then from your dev machine, point publish/generate/client at the VPS instead of local:

```bash
spacetime publish -s <vps-host>:3000 --module-path server hexmerge
spacetime generate --lang rust --out-dir client/src/module_bindings --module-path server
```

And update `HOST` to `http://<vps-host>:3000` (or `https://...` behind TLS,
required once this is served from itch.io) in both `client/src/main.rs` and
`client/src/bin/web.rs`.
