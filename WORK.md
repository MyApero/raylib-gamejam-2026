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

`index.html` is a thin wrapper that embeds two independent copies of the
game (`game.html?slot=1` / `?slot=2`) in iframes, side by side in landscape
and stacked in portrait, so two players can play on one screen. Open
`game.html` directly for a single instance (e.g. while debugging). The
`slot` query param namespaces the SpacetimeDB session token in
`sessionStorage` so the two iframes don't collide on one identity.

The web client can't use spacetimedb-sdk (raylib's web target is
emscripten; the SDK's browser feature needs wasm-bindgen, which doesn't
support emscripten), so it speaks SpacetimeDB's `v1.json.spacetimedb`
WebSocket protocol directly: the socket lives in JS (`client/web/game.html`)
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

## Frontend deployment (`raylib.mister-esman.uk`)

This repo is cloned on the VPS at `~/raylib-gamejam-2026`, and Caddy's site
root for `raylib.mister-esman.uk` points directly at
`~/raylib-gamejam-2026/client/web` — no separate deploy/copy step. To ship
a frontend update:

```bash
# on the VPS
cd ~/raylib-gamejam-2026
git pull
./build-web.sh   # rebuilds web.wasm/web.js and copies them into client/web/ in place
```

Caddy serves the updated static files immediately — no reload needed (that
hot-reload gotcha only applies to editing the Caddyfile itself, see above).

## Connection logging

Player connect/disconnect events are logged in two places:
- Identity-level, from the module itself: `spacetime logs hexmerge -s local`
  (see the `client_connected`/`identity_disconnected` reducers in
  `server/src/lib.rs`). No IP available here — reducers never see it.
- IP-level, from Caddy's access log on `spacetime.mister-esman.uk`
  (`~/caddy/logs/spacetime.log`, root-owned, JSON lines, one per WebSocket
  session). The site is behind Cloudflare, so `client_ip` only resolves to
  the real visitor (rather than Cloudflare's edge) because of the
  `trusted_proxies static <cloudflare ranges>` global option in
  `~/caddy/Caddyfile`. Note: editing `~/caddy/Caddyfile` in place doesn't
  hot-reload via `caddy reload` if the edit was done by rewriting the file
  (new inode) — the container's bind mount stays pinned to the old file
  until `docker compose restart caddy` (or `up -d`) in `~/caddy`.

## Trajectory bots

Two always-on bots (`client/src/bin/bot.rs`) hold the board's `heart-bot`
and `hexagon-bot` players, tracing a parametric heart curve and a hexagon
outline respectively, so it never looks empty. Each keeps its own
persisted identity (`~/.spacetimedb_client_credentials/hexmerge-bot-{heart,hexagon}`)
independent of the human client's.

Running 24/7 via systemd (`/etc/systemd/system/hexmerge-bot@.service`,
`Restart=always`, enabled at boot):

```bash
systemctl status hexmerge-bot@heart.service hexmerge-bot@hexagon.service
journalctl -u hexmerge-bot@heart -f
```

After changing `server/src/lib.rs` or `bot.rs`, rebuild and restart:

```bash
cargo build -p client --bin bot --release
sudo systemctl restart hexmerge-bot@heart.service hexmerge-bot@hexagon.service
```
