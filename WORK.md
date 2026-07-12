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

## Terminal 2 — Client

### Web

```bash
./build-web.sh
python3 -m http.server -d client/web 8080
```

Then open http://localhost:8080 in a browser.


### Native

```bash
cargo run -p client
```

Multiple clients

```bash
HEXEL_PLAYER=p1 cargo run -p client --bin client
HEXEL_PLAYER=p2 cargo run -p client --bin client
```

## VPS setup (SpacetimeDB 2.7, for later)

```bash
curl -sSf https://install.spacetimedb.com | sh
spacetime start --listen-addr 0.0.0.0:3000
```

Then from your dev machine, point publish/generate/client at the VPS instead of local:

```bash
./server/publish.sh
```

## Admin account / "draw anywhere" (F16)

```bash
# local dev instance
cargo run -p client --bin admin_probe -- "<server/.env admin password>"

# your published VPS instead — point at whatever host/port its SpacetimeDB
# actually listens on (see "VPS setup" above)
cargo run -p client --bin admin_probe -- "<the admin password>" http://<vps-host>:3000
```

Paste the printed `token` value into the web client's "Paste an ID" field

## Admin (F10)

Claiming the role is still CLI-only (`claim_admin`/`admin_probe`, above) —
everything past that now has in-game UI (author follow-up, 2026-07-12), on
both clients:

- Admin has no island of their own — `claim_admin` deletes whatever island
  `client_connected` had already handed that identity, and seeds their
  `Inventory` with a full 30-hue spread (every 12°) so every color is
  paintable immediately, not just via `admin_paint_cell`/`admin_erase_cell`
  server-side.
- The footer's "My Isle" button becomes "Config" for admin (they have no
  island to show) — currently just "Refresh isle placement", a manual
  trigger for the same re-sort `rerank_fire` runs periodically.
- The Paint/Erase/Move footer tool cycles into a 4th state, admin-only:
  Edit. While active, tapping/clicking another player's island opens a
  modal to edit their display name/XP and the island's like count (Save),
  or delete the island AND the owner's account entirely (double-click-
  confirmed Delete) — mobile-friendly by design (no right-click/long-press).

Still CLI-only, called against whichever server you're pointed at (swap
`-s local` for the production server name):

```bash
# freeze/unfreeze all player interaction (painting, merging, moving, liking,
# link editing, border editing) — a panic button for active abuse; the admin
# reducers themselves stay callable while frozen
spacetime call hexel set_frozen true -s local
spacetime call hexel set_frozen false -s local

# wipe every painted cell on a given island (moderation for offensive/abusive
# art) — the island row itself (ownership, likes, link, border, slot) is
# untouched, so the owner keeps their spot
spacetime call hexel delete_island_cells <island_id> -s local

# demo/debug only: assign an exact XP total to one uniquely named player.
# 300 XP is level 3, which unlocks the central HEXA feature. The in-game
# Edit modal (above) is the general-purpose way to do this now; this stays
# for scripting/by-name convenience.
spacetime call hexel admin_set_xp_by_name "<display name>" 300 -s local
```



### Backups

#### Save

```sh
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
BACKUP_DIR="$HOME/spacetime-backups"
DATA_DIR="$HOME/.local/share/spacetime/data"
mkdir -p "$BACKUP_DIR"
tar -C "$DATA_DIR" --exclude=cache --exclude=spacetime.pid \
  -cf - control-db program-bytes replicas metadata.toml config.toml \
  | zstd -T0 -19 -q -o "$BACKUP_DIR/hexel-$STAMP.tar.zst"
```

#### Restore

```sh
# 1. Safety snapshot of current state before overwriting it (skippable)
STAMP=$(date -u +%Y%m%dT%H%M%SZ)
DATA_DIR="$HOME/.local/share/spacetime/data"
tar -C "$DATA_DIR" --exclude=cache --exclude=spacetime.pid \
  -cf - control-db program-bytes replicas metadata.toml config.toml \
  | zstd -T0 -19 -q -o "$HOME/spacetime-backups/pre-restore-safety-$STAMP.tar.zst"

# 2. Stop the live server
pkill -f spacetimedb-standalone
while pgrep -f spacetimedb-standalone >/dev/null; do sleep 1; done

# 3. Swap in the backup to restore
rm -rf "$DATA_DIR/control-db" "$DATA_DIR/program-bytes" "$DATA_DIR/replicas" "$DATA_DIR/metadata.toml" "$DATA_DIR/config.toml"
zstd -dc ~/spacetime-backups/hexel-<STAMP>.tar.zst | tar -C "$DATA_DIR" -xf -

# 4. Restart it the same way it was originally running
nohup /home/antoine/.local/share/spacetime/bin/2.6.1/spacetimedb-standalone start \
  --data-dir "$DATA_DIR" \
  --jwt-key-dir ~/.config/spacetime/ \
  > "$DATA_DIR/logs/restart-$(date -u +%Y%m%dT%H%M%SZ).log" 2>&1 &
disown

# 5. Verify
spacetime sql -s local hexel -y "SELECT * FROM island_cell"
```

No automated backup job — restoring "from a backup" (hexel.md's Admin
section) means re-publishing a `spacetime sql` dump. Dump the world state
before anything risky (a schema migration, a manual DB edit):

```bash
for t in config user inventory island island_like island_link_click island_cell margin_cell; do
  spacetime sql hexel -s local "SELECT * FROM $t" > "backup-$t-$(date +%Y%m%dT%H%M%S).txt"
done
```

These are plain-text `spacetime sql` table dumps (for manual inspection/
restore-by-hand), not a reducer-replayable snapshot — there's no automated
restore path, matching plan.md's "Admin tooling is minimal for now" scope.

## Connection logging

Player connect/disconnect events are logged in two places:
- Identity-level, from the module itself: `spacetime logs hexel -s local`
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

Three always-on bots (`client/src/bin/bot.rs`) hold the board's `heart-bot`,
`hexagon-bot`, and `center` players, tracing a parametric heart curve, a
hexagon outline, and a small idle loop at the world origin respectively, so
it never looks empty. Each keeps its own persisted identity
(`~/.spacetimedb_client_credentials/hexel-bot-{heart,hexagon,center}`)
independent of the human client's. The `center` bot's display name is
literally `Merge with me!` — the backlog's "bot at the middle with a
highlight" callout, rendered for free by F12's cursor name labels
(`world::draw_cursor_label`), no bot-specific client code needed.

F12: positions are world-cartesian units (origin = admin's slot-0 island
center), not the old fixed-canvas pixel space — re-verify trajectories with
`spacetime sql -s local hexel "SELECT name, cx, cy FROM user"` after any
geometry-constant change (`ISLAND_RADIUS`/`MARGIN_GAP_TILES` in `shared`).

Running 24/7 via systemd (`/etc/systemd/system/hexel-bot@.service`,
`Restart=always`, enabled at boot):

```bash
systemctl status hexel-bot@heart.service hexel-bot@hexagon.service hexel-bot@center.service
journalctl -u hexel-bot@heart -f
```

After changing `server/src/lib.rs` or `bot.rs`, rebuild and restart:

```bash
cargo build -p client --bin bot --release
sudo systemctl restart hexel-bot@heart.service hexel-bot@hexagon.service hexel-bot@center.service
```

(First deploy of the `center` bot: also `sudo systemctl enable --now
hexel-bot@center.service` once, same as the original two were enabled.)
