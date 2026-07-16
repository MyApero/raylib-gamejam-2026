# Dev PoC — SpacetimeDB 2.6.1 + raylib-rs 6.0

Multiplayer PoC for the raylib 6.x gamejam (theme: **hex + merge**, see `GAMEJAM_RULES.md`).
Each connected client is a hexagon that follows that client's mouse cursor; all clients see
all hexagons in real time. MERGE mechanics are not implemented yet — this is just the
networked-hexagon foundation.

## One-time setup

The prebuilt `spacetime` CLI crashes on Asahi Linux (`jemalloc: Unsupported system page size`,
16K-page kernel). Build it from source instead:

```bash
cargo install --git https://github.com/clockworklabs/SpacetimeDB --tag v2.6.1 --locked \
  spacetimedb-cli spacetimedb-standalone
rustup target add wasm32-unknown-unknown
```

Make sure `~/.cargo/bin` comes before any other `spacetime` binary in `PATH` (e.g. rename
aside a broken prebuilt one at `~/.local/bin/spacetime` if present).

## Run

```bash
spacetime start                             # local instance on http://localhost:3000
./server/publish.sh                         # publish module `hexel`
./generate_module_bindings.sh               # (re)generate client/src/module_bindings
cargo run -p client                         # run 2+ instances to see multiplayer
```

## World timestamp replay

Press the **Replay** button in the game header (native or web) to run the
current retained world as a read-only, whole-map animation. Replay deliberately
shows hidden island borders and disables the low-zoom flat-island optimization
and view culling, so every island and painted tile participates in the reveal.

The native client can also start replay immediately from the command line. The
argument is the number of real seconds used to travel from the oldest retained
timestamp to the newest:

```bash
cargo run -p client --bin client -- --replay 30
# equivalent: HEXEL_REPLAY_SECONDS=30 cargo run -p client --bin client
```

During playback, use the on-screen **Slower**, **Pause**, **Faster**, **Restart**,
and **X** controls; the native/desktop keyboard equivalents are `-`, `Space`,
`+`, `R`, and `Escape`. At 100% it becomes a live viewer and newly painted
cells appear as their subscription updates arrive.

This first replay uses the timestamps available through normal subscriptions:
island creation and each currently retained tile's latest paint. SpacetimeDB's
internal commitlog also retains overwritten colors and erased tiles for database
recovery, but it does not expose that log as a historical client subscription;
recovering those intermediate states requires a separate commitlog exporter or
an application-level paint-history table.

Re-run `generate_module_bindings.sh` after any change to `server/src/lib.rs`.

## HEXA demo bots

Build and launch five assistants, leaving the sixth formation slot open for
a player:

```bash
cargo build -p client --bin bot --release
for n in {1..5}; do
  ./target/release/bot "assist-$n" >"/tmp/hexa-bot-$n.log" 2>&1 &
done
```

Each bot eases between a distinct corner of the central island and its
centre. They hold the centre for three seconds, return to their corners, and
reset once when launched so every new showcase starts with fresh colors.
They do not reset between passes, which preserves their promoted HEXA level.
Reset confirmations and failures are written to the corresponding log file.

To showcase HEXA, first claim admin as described in `WORK.md`. Wait until
`spacetime sql hexel "SELECT name, xp FROM user"` lists `Hexa bot 1` through
`Hexa bot 5`, then promote them to level 3 (300 XP):

```bash
for n in {1..5}; do
  spacetime call hexel admin_set_xp_by_name "Hexa bot $n" 300 -s local
done
```

Promote yourself with the same command, replacing the name with your unique
in-game display name. `spacetime sql hexel "SELECT name, xp FROM user"` shows
the exact names currently connected.

Stop the assistants with:

```bash
pkill -f 'target/release/bot assist-'
```

## Useful

```bash
spacetime logs hexel
spacetime sql hexel "SELECT * FROM user"
```

## Troubleshooting

- `spacetime start` can't find the standalone binary → run
  `~/.cargo/bin/spacetimedb-standalone start` directly.
- `publish` asks about login for local server → `spacetime login --server-issued-login local`
  (or the offered guest/local option).
- raylib-sys build errors → missing system GL/Wayland dev packages
  (e.g. on Fedora/Asahi: `mesa-libGL-devel`, `libxkbcommon-devel`, `wayland-devel`,
  `libX11-devel`).
- Client fails to compile against `module_bindings` → bindings were generated with the wrong
  CLI version; they must come from the v2.6.1 CLI (this exact mismatch is what currently
  blocks the `albert` repo's client on its `update-asahi` branch).

## Verification

1. `spacetime logs hexel` shows a `client_connected` line after a client starts.
2. `spacetime sql hexel "SELECT * FROM user"` shows one row per connected client with
   live `x`/`y`.
3. Run **two** `cargo run -p client` instances side by side: each window shows **two
   hexagons**; moving the mouse in one window moves that hexagon in *both* windows (your own
   hexagon has a black outline).
4. Close one client → its hexagon disappears from the other window (filtered on `online`).

## Post-PoC risks (not addressed yet)

- **Web build feasibility**: raylib's web target is `wasm32-unknown-emscripten`, while
  `spacetimedb-sdk`'s browser support targets `wasm32-unknown-unknown`. Whether both can
  coexist in one binary needs verification before the jam entry can ship — the jam requires
  a WebAssembly build playable on itch.io.
- **Hosting**: a real submission needs a publicly reachable SpacetimeDB instance served over
  `wss://` (itch.io is HTTPS), kept up through the 6-day voting window.
- MERGE mechanic design is still open.
