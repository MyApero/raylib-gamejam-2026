# Running the PoC

## Setup (once per machine)

```bash
curl -sSf https://install.spacetimedb.com | sh
rustup target add wasm32-unknown-emscripten
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

The web client can't use spacetimedb-sdk (raylib's web target is
emscripten; the SDK's browser feature needs wasm-bindgen, which doesn't
support emscripten), so it speaks SpacetimeDB's `v1.json.spacetimedb`
WebSocket protocol directly: the socket lives in JS (`client/web/index.html`)
and the game drains its pushed messages once per frame. See
`client/src/bin/web.rs`.

### Testing on your phone (same Wi-Fi)

Both `http.server` and `spacetime start` bind all interfaces by default, so
they're already reachable from other devices on the same network — no
firewall/config changes needed on a typical setup. Find your machine's LAN
IP (`ip -4 addr` or similar) and open `http://<lan-ip>:8080` on the phone.
The web client resolves SpacetimeDB's address from the page's own hostname
at runtime, so this works without editing any code. Single-finger touch
drags move your hexagon (raylib translates single-touch to mouse position
on the web platform); the canvas is CSS-scaled to fit the screen without
zooming.

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

And update `HOST` in `client/src/main.rs` to `http://<vps-host>:3000` (or
`https://...` behind TLS, required once this is served from itch.io).
`client/src/bin/web.rs` needs no change — it resolves SpacetimeDB's host
from the page's own hostname at runtime — but if the VPS's SpacetimeDB
port ever differs from 3000, update `SPACETIMEDB_PORT` there.
