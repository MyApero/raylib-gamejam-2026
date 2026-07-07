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
./server/publish.sh                         # publish module `hexmerge`
./generate_module_bindings.sh               # (re)generate client/src/module_bindings
cargo run -p client                         # run 2+ instances to see multiplayer
```

Re-run `generate_module_bindings.sh` after any change to `server/src/lib.rs`.

## Useful

```bash
spacetime logs hexmerge
spacetime sql hexmerge "SELECT * FROM user"
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

1. `spacetime logs hexmerge` shows a `client_connected` line after a client starts.
2. `spacetime sql hexmerge "SELECT * FROM user"` shows one row per connected client with
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
