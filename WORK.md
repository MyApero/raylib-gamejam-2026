# Running the project

hexmerge — a multiplayer raylib-rs + SpacetimeDB gamejam entry, playable in
the browser. Each connected client is a hexagon that follows that client's
mouse cursor; all clients see all hexagons in real time.

One-time dependency setup lives in [INSTALLATION.md](INSTALLATION.md).

## Terminal 1 — SpacetimeDB

```bash
spacetime start
```

Keep this running. First time only (and again any time `server/src/lib.rs` changes):

```bash
./server/publish.sh
```

## Terminal 2 — web client

Requires emsdk activated (`emsdk_env.sh` sourced) — `build-web.sh` does this
for you (assumes `../emsdk`, override with `EMSDK_DIR=...`).

```bash
./build-web.sh
python3 -m http.server -d client/web 8080
```

Then open http://localhost:8080 in a browser. Debug builds crash the
emscripten linker (binaryen assertion), so the script always builds
`--release`.

`index.html` is a thin wrapper that embeds two independent copies of the
game (`game.html?slot=1` / `?slot=2`) in iframes, side by side in landscape
and stacked in portrait, so two players can play on one screen. Open
`game.html` directly for a single instance (e.g. while debugging). The
`slot` query param namespaces the SpacetimeDB session token in
`sessionStorage` so the two iframes don't collide on one identity.

The web client speaks SpacetimeDB's `v1.json.spacetimedb` WebSocket
protocol directly: the socket lives in JS (`client/web/game.html`) and the
game drains its pushed messages once per frame. See
`client/src/bin/web.rs`.

## Verification

1. `spacetime logs hexmerge` shows a `client_connected` line after a client starts.
2. `spacetime sql hexmerge "SELECT * FROM user"` shows one row per connected client with
   live `x`/`y`.
3. Open `index.html` (or two `game.html` tabs): each pane shows **two
   hexagons**; moving the mouse in one pane moves that hexagon in *both*
   panes (your own hexagon has a black outline).
4. Close one tab → its hexagon disappears from the other pane (filtered on presence timeout).

## Useful SpacetimeDB commands

```bash
spacetime logs hexmerge
spacetime sql hexmerge "SELECT * FROM user"
```

## Troubleshooting

- `spacetime start` can't find the standalone binary → run
  `~/.cargo/bin/spacetimedb-standalone start` directly.
- `publish` asks about login for local server → `spacetime login --server-issued-login local`
  (or the offered guest/local option).
- emsdk-related build errors → make sure `emsdk_env.sh` is sourced (or use
  `./build-web.sh`, which does it for you), see [INSTALLATION.md](INSTALLATION.md).

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
