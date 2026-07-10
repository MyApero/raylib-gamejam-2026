# Running the project

hexmerge — a multiplayer raylib-rs + SpacetimeDB gamejam entry, playable in
the browser. Each client wakes up in a single dark room (bigger than the
window, followed by a camera), can only half-see without the flashlight, and
must collect 6 randomly-colored triangles scattered around the room, drag
them onto a merge table's hexagon (matching colors to slots) to unlock the
door and escape. Player positions are synced live over SpacetimeDB; all
clients see each other move in real time.

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
3. **Camera**: the room is bigger than the 720x720 window; walking (ZQSD) pans the
   camera, clamped so no area outside the room is ever visible.
4. **Lighting**: without the flashlight, only a faint halo around the player is
   visible. Holding `F` extends vision into a fading cone pointed in the last
   movement direction, well short of lighting the whole room; the cone rotates
   as the movement direction changes and holds steady when idle.
5. **Triangles**: walking over one picks it up (no flashlight required); the HUD
   counter goes up to 6/6.
6. **Merge table**: `E` toggles it; the hexagon shows 6 slots tinted with their
   required color (duplicates possible); dragging an inventory triangle onto a
   slot of the matching color places it (wrong color / already-filled slot
   rejects the drop, shown via a red outline while hovering); completing all 6
   unlocks the door.
7. **Door + escape**: locked beforehand (blocks movement into it); once unlocked,
   walking through triggers an "Échappé !" overlay.
8. Open `index.html` (or two `game.html` tabs): each pane shows **two
   players**; moving in one pane moves that player in *both* panes when
   within the other player's lit range (your own player is always visible to
   yourself, drawn in white).
9. Close one tab → its player disappears from the other pane (filtered on presence timeout).

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
