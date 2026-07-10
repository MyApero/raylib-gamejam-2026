# Running the project

hexmerge — a multiplayer raylib-rs + SpacetimeDB gamejam entry. Each
connected client is a hexagon that follows that client's mouse cursor; all
clients see all hexagons in real time.

One-time dependency setup lives in [INSTALLATION.md](INSTALLATION.md).

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

## Verification

1. `spacetime logs hexmerge` shows a `client_connected` line after a client starts.
2. `spacetime sql hexmerge "SELECT * FROM user"` shows one row per connected client with
   live `x`/`y`.
3. Run **two** `cargo run -p client` instances side by side: each window shows **two
   hexagons**; moving the mouse in one window moves that hexagon in *both* windows (your own
   hexagon has a black outline).
4. Close one client → its hexagon disappears from the other window (filtered on `online`).

## Useful SpacetimeDB commands

```bash
spacetime logs hexmerge
spacetime sql hexmerge "SELECT * FROM user"
```

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
