# Hexaworld — Implementation Plan

Design source: `Hexaworld.md` (+ clarifications from the author, folded in below).
Protocol: this file is written by the planner; the executor implements it feature by
feature, in strict batches of 1–2 tasks, and records evidence in `status.md` using the
tags `VERIFIED` / `REASONED` / `ASSUMED`. The author hand-tests each feature before the
next one starts.

## Executor ground rules

- Repo: `~/delivery/perso/raylib-gamejam-2026`, branch `hexaworld`. Read target files
  before editing them.
- Shell is `zsh`; Fedora package management is `dnf5` only.
- After ANY change to `server/src/lib.rs`: run `./server/publish.sh` then
  `./generate_module_bindings.sh`. Bindings MUST come from the v2.6.1 CLI
  (`~/.cargo/bin/spacetime`).
- NEVER hand-edit `client/src/module_bindings/**` — it is generated.
- The web client does NOT use the Rust SDK. It speaks the raw `v1.json.spacetimedb`
  WebSocket protocol via `client/web/game.html` (socket in JS) and
  `client/src/bin/web.rs` (message draining + game loop). Every new table and every new
  reducer must be mirrored there by hand — this is feature F5, do not drift it earlier
  or later.
- Hard jam constraints: window stays 720×720; web build (`./build-web.sh`) must keep
  compiling at every feature boundary; total web package must stay < 64 MB.
- One commit per feature minimum, professional conventional messages
  (`feat: ...`, `fix: ...`), no AI attribution or AI names anywhere in commits.
- Deadline: submission 2026-07-12 18:00 UTC — F7 (deployed, submittable) must land on
  2026-07-11; everything after F7 is optional redeploys. If the schedule slips, P1/P2
  are cut, never F1–F7.

## Fixed design decisions (do not re-litigate)

1. Cursor positions ARE sent to the server (throttled). Your own cursor is *drawn* from
   the local position for zero latency; the network copy exists for other players and
   for server-side merge detection.
2. Merge result = circular mean of the two brush hues, computed server-side.
3. A player paints ONLY on their own island. The margins between islands are free
   pixel-war zones paintable by everyone.
4. Island ranking (likes → distance to center) is recalculated every 5 minutes, with a
   5-second on-screen countdown before islands are repositioned. (P1)
5. Merge is instant, no cooldown. If the two hues are equal, no merge happens (this
   makes merging naturally idempotent: after a merge both players hold the same hue, so
   resting cursors don't re-trigger). Players with Lock enabled never merge.
6. A merged color is "new" *per player*: each of the two participants who doesn't
   already own the hue gets it added to their inventory (with timestamp + partner
   identity) and gets the XP credit.
7. Hues are quantized to integer degrees, `0..=359`. That is the unlockable resource.
8. Level unlocks a *maximum* saturation; luminosity (HSV value) is always free.
9. XP amounts are constants in one place, tuned later by playtesting.
10. Island = hexagon of side 14 → radius 13 → 547 tiles. One island per player,
    created at first connection. Admin island reserved at the world center (slot 0).
    (Superseded by decision 20, 2026-07-11: slot 0 is now the ownerless community
    canvas, not a personal island — admin is no longer physically placed there.)
11. The reconnection identifier is the SpacetimeDB token (Cookie-Clicker style import),
    persisted in `localStorage` (works inside itch.io's game iframe; strict-privacy
    browser modes may partition/purge it, and the itch embed and the standalone site
    are different origins with separate storage — the import field in F6 is the
    official recovery path for all of these). Impersonation by token leak is accepted
    for the jam.
12. Account reset keeps identity + name + island art, but wipes XP and inventory and
    rolls a new random start hue. (ASSUMED: island art survives reset — confirm with
    author at F6.)
13. Admin tooling is minimal for now (P2). Backups = documented CLI dumps on the VPS.
14. A player's link is island-level (not per-tile) and is stored as just the itch.io
    submission id (`u32`); the client renders it as
    `https://itch.io/jam/raylib-6x-gamejam/rate/<id>`.
15. Long-press on any painted tile merges your brush hue with that tile's hue
    ("obtained with" = that tile's painter). This keeps the theme mechanic experiencable
    by a solo rater — jam-critical, so it is P0.
16. Color space is HSV (raylib native). OKLCh is at most a P2 rendering swap; the
    canonical stored model stays `(hue u16, sat u8, val u8)` regardless.
17. (2026-07-11, supersedes the F8 hand-test choice) Island info opens on **hover**
    over a foreign island on desktop; double-click-to-like stays exactly as shipped;
    touch is unchanged (tap opens, double-tap likes). The F8 click-to-open version
    "turned out to feel wrong in practice" — the exact reopening condition the backlog
    reserved.
18. (2026-07-11) Eraser reverts a cell to the unpainted state (the cell row is
    deleted), is permitted exactly where painting is permitted (own island interior +
    margin), and consumes paint budget like a normal paint.
19. (2026-07-11) Middle-click eyedropper adopts a tile's color only if that tile's hue
    is already in the caller's inventory; it never grants hues — long-press merge
    remains the only acquisition path. Sat is re-clamped to the caller's cap.
20. (2026-07-11, author ruling — resolves the backlog's "center-island identity"
    question) Slot 0 (world center) is a permanent, ownerless COMMUNITY island:
    anyone connected may paint/erase there, not just its "owner" — decision 3's
    own-island-only rule is superseded for this one island specifically; the margins
    clause is unaffected. It exists from the first `client_connected` after this
    feature ships (lazy-seeded, same convention `config`/the scheduled-reducer rows
    already use, so a `--delete-data=never` republish of an existing database doesn't
    need a fresh `init` to get it). Ownership is represented by the sentinel
    `Identity::ZERO` (never a real connecting player's identity) rather than
    `Option<Identity>`, to avoid changing the `owner` column's type everywhere it's
    already read. Consequence for decision 10 / F10: `claim_admin` no longer relocates
    the claimant's island to slot 0 — admin is now purely a role (freeze/moderation
    powers), decoupled from any physical island. Its info popup/hover-tooltip work
    like any other island's, labeled **"Free Isle"** (author-requested, 2026-07-11)
    rather than a real player's name or the generic "another player" fallback — both
    clients' `player_label` special-cases the sentinel identity. Its unpainted tiles
    render white (author-requested, same date), not the usual gray placeholder, so
    it reads as the shared canvas at a glance even before anyone's painted on it.
    Long-press tile-merge and the eyedropper work on it like any other painted cell —
    it's the theme's shared canvas, not a rateable profile, though liking/XP-crediting
    it is a harmless no-op left as-is (no real player ever owns it to receive the XP).

## Canonical constants — single source of truth

Mirror these EXACTLY in: server module, native client, web client (JS + `web.rs`).
Put them in one Rust module per binary (`shared` values re-declared, with a comment
pointing here).

| Constant | Value | Meaning |
|---|---|---|
| `ISLAND_RADIUS` | 13 | hex distance from island center to edge (side 14, 547 cells) |
| `MARGIN_GAP_TILES` | 6 | gap (fine hex tiles) left between neighboring islands' paintable interiors — MUST be even, since the placement radius is bumped by `MARGIN_GAP_TILES / 2` and that bump contributes symmetrically from both neighbors (raised from 2 — author wanted more pixel-war breathing room) |
| `SLOT_SPACING` | 33 | fine-hex distance between adjacent island slot centers, on the "hex-of-hexes" tiling basis added at F9.5 (`geometry::SLOT_U`/`SLOT_V`, generated from `ISLAND_RADIUS + MARGIN_GAP_TILES / 2`) — flat sides face flat sides with a uniform `MARGIN_GAP_TILES`-tile gap, not the old same-axis scaling's inescapable triangular gaps at every spacing value |
| `MERGE_DIST` | 2.0 | cursor-merge trigger distance, world units (raised from 1.0 — deployed-build testing found the range too short; author tunes by feel at F9.5, keep this row in sync with the settled value) |
| `PAINT_BUCKET_MAX` | 1000.0 | rate limit: bucket capacity (raised 50x from the original 20.0 — felt too restrictive in hand-testing) |
| `PAINT_REFILL_PER_SEC` | 50.0 | rate limit: 1000 tiles / 20 s (raised 50x from the original 1.0, same ratio) |
| `PRESENCE_TIMEOUT` | 3 s | cursor shown / merge-eligible if `last_seen` fresher than this |
| `CURSOR_SEND_HZ` | 20 | max `set_pos` rate, and only when moved |
| `XP_MERGE_NEW` | 25 | XP per newly unlocked hue |
| `XP_LIKE` | 10 | XP to island owner per like (P1) |
| `XP_LINK_CLICK` | 5 | XP to island owner per link click (P1) |
| `XP_TIME` | 1 | passive XP per `TIME_XP_PERIOD_SECS` tick, to every present user (P1, F9) |
| `TIME_XP_PERIOD_SECS` | 60 | time-XP tick interval (P1, F9) |
| `LEVEL_XP` | 100 | level = xp / LEVEL_XP |
| `SAT_CAP(level)` | `min(100, 40 + 3*level)` | max brush saturation, percent |
| `LONG_PRESS_MS` | 400 | tile-merge hold duration |
| `LONG_PRESS_SLOP_PX` | 8 | max pointer travel during a long press |
| `RERANK_PERIOD` | 300 s | island re-ranking interval (P1) |
| `RERANK_WARNING` | 5 s | countdown before islands move (P1) |
| `HUE_TOLERANCE` | 5° | hue-slider nudge range (added in F4); every "same hue" comparison uses circular distance ≤ this, never exact equality |
| `HEXA_SIZE` | 6 | cursors needed to ignite a Hexa event (P2) |
| `HEXA_RADIUS` | 2.0 | cluster radius for Hexa detection, world units (P2) |
| `XP_HEXA` | 150 | one-time-per-player Hexa bonus (P2) |
| `HEXA_SWEEP_PERIOD_SECS` | 2 | `hexa_cluster` stale-row safety-net sweep interval, server-only (P2, F13 follow-up) |
| `GIFT_SPAWN_PERIOD_SECS` | 45 | flying-gift spawn/expire tick interval, server-only (P2, F11) |
| `GIFT_LIFETIME_SECS` | 25 | how long an unclaimed gift lasts before the tick sweeps it (P2, F11) |
| `GIFT_DRIFT_RADIUS` | 1.2 | world-unit radius of the gift's circular drift around its spawn point — shared client/server (P2, F11) |
| `GIFT_DRIFT_PERIOD_SECS` | 5.0 | seconds per full drift loop — shared client/server (P2, F11) |
| `GIFT_CLAIM_DIST` | 3.0 | world units, server-enforced claim range from the gift's current drifted position — shared client/server (P2, F11) |
| `XP_GIFT` | 20 | flat XP on the claim's "XP" branch (and the "hue" branch's own fallback if all 8 rerolls collide) (P2, F11) |

## Geometry spec (flat-top hexes, axial coordinates)

World units: 1.0 = one hex outer radius. All cursor traffic and merge distances are in
world cartesian units. On-screen size comes from the camera zoom only.

- Axial `(q, r)`, flat-top. Cartesian center of a hex:
  `x = 1.5 * q`, `y = sqrt(3) * (r + q/2)`.
- Hex distance: `hexdist(a, b) = (|dq| + |dr| + |dq + dr|) / 2` with `dq = a.q - b.q`,
  `dr = a.r - b.r`.
- Pixel → hex: convert to fractional axial (`q = x/1.5`, `r = y/sqrt(3) - q/2`), then
  cube-round (standard algorithm: round q, r, s = -q-r; fix the component with the
  largest rounding error so q+r+s = 0).
- Axial directions, in order: `E(+1,0) SE(+1,-1) NW(0,-1) W(-1,0) SW(-1,+1) NE(0,+1)`
  — any consistent order is fine as long as ring/spiral enumeration uses one order
  everywhere.
- **Slot lattice**: island slot with coarse axial `(Q, R)` has its center at fine axial
  `(29*Q, 29*R)`. All 6 coarse neighbors are exactly 29 fine hexes away.
- **Spiral slot order**: slot 0 = `(0,0)` (admin). Slots 1.. follow concentric coarse
  rings (ring 1 = 6 slots, ring 2 = 12, ...), enumerated with the standard hex ring
  walk (start at `k * direction[4]`, walk k steps in each of the 6 directions). The
  mapping `slot_index -> (Q, R)` must be a pure function, identical in server and both
  clients.
- **Island interior**: fine cells with `hexdist(cell, slot_center) <= 13`. Owner-only.
- **Margin (pixel-war zone)**: any fine cell with `hexdist > 13` from EVERY slot
  center (occupied or not — islands always sit exactly on slot centers, so future
  islands can never cover margin art), AND within `29 * (occupied_rings + 1)` of the
  origin (keeps the canvas bounded). To test membership cheaply: coarse candidate
  `(Q, R) = cube_round(q/29, r/29)`; check that candidate and its 6 coarse neighbors.
- **Cell id packing** (u32 primary keys):
  - island cell: `(island_id << 10) | ((q_local + 16) << 5) | (r_local + 16)` with
    `q_local, r_local ∈ [-13, 13]` relative to the island center.
  - margin cell: `((q + 512) << 10) | (r + 512)` with world `q, r ∈ [-512, 511]`.

## Color spec

- Stored color everywhere: packed u32 `(h << 16) | (s << 8) | v` with
  `h ∈ 0..=359`, `s, v ∈ 0..=100`.
- HSV→RGB only at render time (native: `Color::color_from_hsv`; web: standard JS
  conversion, S/V scaled to 0..1).
- **Merge formula** (server): given integer hues `h1 != h2`, result
  `h = round(atan2(sin(h1) + sin(h2), cos(h1) + cos(h2)))` in degrees, normalized to
  `0..=359`. Degenerate case: if `(h1 - h2) mod 360 == 180`, the mean is undefined —
  use `(min(h1, h2) + 90) mod 360`. Equal hues: reducer returns an error, no merge.

## Server data model (target state after F1)

Tables (all `public` unless noted):

- `config` (single row, id 0): `frozen: bool`, `admin: Option<Identity>`,
  `next_rerank_at: Option<Timestamp>` (P1).
- `user`: `identity` (pk), `name: Option<String>`, `online: bool`,
  `cx: f32, cy: f32` (cursor, world cartesian), `last_seen: Timestamp`,
  `hue: u16, sat: u8, val: u8` (current brush), `locked: bool`, `xp: u64`,
  `paint_tokens: f32`, `tokens_at: Timestamp`.
- `inventory`: `id: u64` (pk, auto_inc), `owner: Identity` (btree index),
  `hue: u16`, `obtained_at: Timestamp`, `obtained_with: Option<Identity>`
  (None for the starting hue).
- `island`: `id: u32` (pk, auto_inc), `owner: Identity` (unique), `slot: u32` (unique),
  `likes: u32`, `itch_rate_id: Option<u32>`, `created_at: Timestamp`.
- `island_cell`: `id: u32` (pk, packed), `island_id: u32` (btree), `q: i32, r: i32`
  (local), `color: u32` (packed hsv), `painted_by: Identity`, `painted_at: Timestamp`.
- `margin_cell`: `id: u32` (pk, packed), `q: i32, r: i32` (world), `color: u32`,
  `painted_by: Identity`, `painted_at: Timestamp`.
- P1 additions: `island_like` (unique (island_id, liker)), scheduled tables for
  re-rank and time-XP.

Reducers (P0): `client_connected` (upsert user; create island at next free slot ≥ 1 if
none; seed inventory with one random start hue if empty; set brush to it),
`client_disconnected`, `set_pos(cx, cy)` (+ merge detection), `set_name(String)`,
`set_brush(hue, sat, val)` (validate: hue in caller's inventory, `sat <= SAT_CAP(level)`,
`val <= 100`), `paint_island_cell(q_local, r_local)`, `paint_margin_cell(q, r)`,
`merge_with_cell(cell_kind: u8, cell_id: u32)`, `set_lock(bool)`, `reset_account()`.

Validation rules shared by both paint reducers: caller's user row exists; token bucket
has ≥ 1 token after refill (`tokens = min(20, tokens + elapsed_s * 1.0)`), else error
`"rate limited"`; color written is the caller's CURRENT BRUSH read server-side (clients
never send colors — single anti-cheat point). `paint_island_cell` additionally: caller
owns an island and `hexdist((q,r), (0,0)) <= 13`. `paint_margin_cell` additionally: the
margin membership + bound rules from the geometry spec.

Merge procedure (shared by cursor-merge and tile-merge): given two hues `h1 != h2` and
two identities A (caller) and B (partner or tile painter): compute merged hue `h`; for
each of A, B: if `h` not in their inventory → insert inventory row
(`obtained_with` = the other identity, `obtained_at` = now) and add `XP_MERGE_NEW`.
Cursor-merge only: set BOTH users' brush hue to `h` (keep each player's own sat/val,
re-clamped to their cap). Tile-merge: set only the caller's brush hue; the tile and its
painter's brush are untouched.

Cursor-merge detection runs inside `set_pos`: after updating the caller's position,
scan users where `online`, `last_seen` fresh (< 3 s), not locked (neither side), not
the caller, cartesian distance < `MERGE_DIST`, and brush hue differs → run the merge
procedure with the nearest such user only.

---

# P0 — submittable game

## F1 — Server: full Hexaworld schema + reducers

**Goal**: the entire P0 data model and rules live in the module; clients come later.

Tasks:
1. Rewrite `server/src/lib.rs` to the data model + reducers above (drop the old
   `Cell` table and the 21×17 bounds; drop `paint_cell`). Implement the geometry
   helpers (hexdist, cube-round, slot spiral, packing) and the merge formula inside the
   module — no external crates beyond what's already in `server/Cargo.toml` plus `libm`
   if needed for `atan2` in wasm (justify in status.md if added).
2. Publish + regenerate bindings. The native client will no longer compile against the
   new bindings — that is EXPECTED and fixed in F2; do not patch the client here beyond
   `cargo check -p server`-level hygiene. Leave the client broken at the end of F1 only
   if the same batch continues into F2; otherwise gate the bindings regen to the start
   of F2 so the tree always builds. Prefer: do F1 + F2 as consecutive batches, regen at
   F2 start.

Files: modify `server/src/lib.rs`, `server/Cargo.toml` (only if a math crate is truly
needed). Do NOT touch: `client/**`, `build-web.sh`, generated bindings (until F2).

Verify (record in status.md):
- `./server/publish.sh` succeeds against local `spacetime start`.
- `spacetime sql hexmerge "SELECT * FROM island"` and each other table: schema exists.
- `spacetime call hexmerge set_name '"x"'` returns the "unknown user" error path
  (proves wiring; CLI calls don't run `client_connected`).
- Full behavioral verification is explicitly deferred to F2's two-client test; list
  the F1 rules as `REASONED` until then, then upgrade them.

## F2 — Native client: world rendering, camera, painting

**Goal**: `cargo run -p client` shows the world of islands; you can pan/zoom, paint
your island and the margins, and see other cursors.

Tasks:
1. Replace `client/src/hexgrid.rs` with `client/src/world.rs`: geometry spec above
   (axial math, slot spiral, packing/unpacking, hsv packing, HSV→RGB via raylib),
   constants table. Regenerate + adopt new bindings; rewrite `client/src/main.rs`.
2. Camera: `Camera2D`; mouse wheel = zoom toward cursor (clamp 0.25–4.0); SHIFT+drag
   or middle-drag = pan. Start centered on own island once it appears in the
   subscription. Render only cells whose slot/coarse neighborhood intersects the view.
   Draw island interiors (owner's island subtly outlined), margin cells, unpainted
   island cells as light placeholder fill, void as background. Own cursor drawn from
   local mouse (own color, small hex outline); other fresh cursors from `user` rows.
3. Input: left-drag paints (calls the matching paint reducer only when the hovered
   cell is paintable by you: own island interior or margin; skip repeats of the same
   cell within a stroke, client-side ~15/s cap under the server's bucket).
   `set_pos` throttled per `CURSOR_SEND_HZ`, world cartesian coords, only when moved.
4. Temporary brush UI (replaced in F3): keyboard cycles inventory hues, +/- adjusts
   value; enough to hand-test painting.

Files: create `client/src/world.rs`; modify `client/src/main.rs`,
`client/src/module_bindings/**` (generated only). Delete `client/src/hexgrid.rs` (fold
anything still useful into `world.rs`). Do NOT touch: `client/src/bin/web.rs`,
`client/web/**`, `client/src/bin/bot.rs` (bot will not compile — allowed to stub its
`main` behind a clearly-marked `todo!()`-free minimal fix ONLY if it blocks the
workspace build; otherwise leave for F7).

Verify:
- `cargo build -p client --bin client` (or default bin) passes.
- Two native instances side by side: each sees its own island + the other's island,
  the other's cursor moves live, painting own island works, painting the OTHER
  player's island is rejected (server error visible in logs / no cell change).
- Rate limit: hold-drag furiously → after ~20 cells, paints stop, resume ~1/s.
  `spacetime sql hexmerge "SELECT COUNT(*) FROM island_cell"` confirms counts.
- Upgrade the F1 `REASONED` items exercised here to `VERIFIED`.

## F3 — Native client: color picker, inventory, footer/header HUD

**Goal**: the real color UX. Footer: center-on-island button, last-3-colors hexes,
inventory button, name field, Lock toggle. Header: short identity + level + XP,
connected/total player counts. Inventory overlay: unlocked hues (rendered at your sat
cap), click to select; luminosity slider; saturation slider capped at `SAT_CAP(level)`.

Tasks:
1. `client/src/ui.rs`: immediate-mode widgets (rects + text, raylib primitives only —
   no new deps). Footer ~44 px band, header ~28 px band; overlay is modal (map input
   suppressed while open). Exact pixel layout is the executor's choice; must fit
   720×720 with no overlap.
2. Wire to reducers: `set_brush`, `set_name`, `set_lock`. Last-3 = client-side ring
   buffer of brush hues actually used. Center button = camera jump to own island.
   Counts: `online` count and total row count of `user`.

Files: create `client/src/ui.rs`; modify `client/src/main.rs`, `client/src/world.rs`
(shared layout constants). Do NOT touch: server, web files.

Verify:
- Manual: pick each inventory hue, adjust sliders, paint → colors match; sat slider
  refuses to exceed cap; name persists across restart
  (`spacetime sql hexmerge "SELECT name FROM user"`).
- `cargo build -p client` clean.

## F4 — Merge, both kinds (native)

**Goal**: the theme. Cursor-merge and long-press tile-merge, with visible feedback.

Tasks:
1. Client feedback: watch your own `inventory` inserts → toast "new color, obtained
   with <name>" + flash the new hue; brush switches automatically (server already set
   it for cursor-merge; for tile-merge update selection on the reducer's success).
2. Long-press (mouse: hold LMB ≥ 400 ms within 8 px on a painted cell you're NOT
   painting — i.e. any cell not paintable by you, or any painted cell when the brush
   wouldn't change it) → `merge_with_cell`. Visual hold-progress ring on the cursor.
3. Lock toggle from F3 respected: verify no cursor-merge when either side is locked.

Files: modify `client/src/main.rs`, `client/src/ui.rs`. Server should already be
complete from F1; if a rule gap is found, fix `server/src/lib.rs` + republish + regen
and note it in status.md.

Verify (two native instances):
- Different brush hues, cursors touch → both get the SAME new hue, both inventories
  gain a row with the partner's identity, XP +25 each,
  `spacetime sql hexmerge "SELECT hue FROM inventory"` shows it once per player.
- Immediately re-touch → no second merge (same hue). Lock one side → no merge.
- Long-press a tile painted by player B as player A → A gains B's tile hue merged with
  A's brush; B's inventory also gains it if new for B.
- 180°-apart hues (set via two accounts' start hues or repeated merges — if
  impractical, `REASONED` from the formula) → deterministic result, no panic.

## F5 — Web client parity

**Goal**: everything F2–F4 works in the browser (the judged target), mouse AND touch.

Tasks:
1. `client/src/bin/web.rs`: mirror the new tables (user, island, island_cell,
   margin_cell, inventory, config) in the row-parsing layer, mirror all new reducer
   calls, port the render/camera/input/UI code (reuse `world.rs`/`ui.rs` — they must
   stay platform-clean: raylib only, no SDK types in their public interfaces).
2. `client/web/game.html`: subscription list update, reducer-call JS plumbing,
   identity/token storage moved from `sessionStorage` to `localStorage` (single-player
   page; the dual-iframe `index.html` demo keeps working via its existing `slot`
   namespacing — do not break it, it's the local two-player test harness).
3. Touch: one-finger tap/drag = paint (and cursor position); one-finger hold ≥ 400 ms
   = tile-merge; two-finger = pan + pinch-zoom. Keep raylib's touch→mouse translation
   for the one-finger path; implement two-finger gestures from raw touch points.
4. `./build-web.sh` stays release-only (debug crashes the emscripten linker — known).

Files: modify `client/src/bin/web.rs`, `client/web/game.html`, `client/web/index.html`
(only if the wrapper needs param plumbing). Do NOT touch: server, native main.

Verify:
- `./build-web.sh` succeeds; `python3 -m http.server -d client/web 8080`.
- Dual-iframe page: paint + cursor-merge between the two iframes works.
- Phone on LAN: pinch-zoom, two-finger pan, paint, long-press merge all work.
- Package size: `du -sh client/web` well under 64 MB.

## F6 — Identity: token login, reset, persistence

**Goal**: Cookie-Clicker-style account portability + reset.

Tasks:
1. Web: header shows short identity; "copy ID" button (full token via JS clipboard —
   inside itch.io's iframe `navigator.clipboard` may be denied, so always provide a
   fallback that shows the token in a selectable text field for manual copy).
   Settings row with a paste-token field → stores to `localStorage`, reconnects as
   that identity (page reload is acceptable). Native: `SetClipboardText` for copy;
   token import optional (web is the judged target — mark native import SKIPPED if
   time-boxed out).
2. `reset_account` reducer button (double-click/confirm to arm): wipes XP + inventory,
   rolls new random start hue, keeps name + island art (decision 12; confirm the
   island-art assumption with the author when they hand-test).
3. Confirm refresh/reopen keeps identity (no total-player inflation).

Files: modify `client/web/game.html`, `client/src/bin/web.rs`, `client/src/ui.rs`,
`client/src/main.rs`; server only if `reset_account` needs a fix.

Verify:
- Browser: note identity → clear tab → reopen → same identity. Copy token, open a
  private window, import token → same account, `SELECT COUNT(*) FROM user` unchanged.
- Reset: inventory has exactly 1 new hue after, XP 0, island art intact.

## F6.5 — Pre-submission bug sweep — ABSORBED INTO F9.5 (2026-07-11)

> Tasks 1–3 below never ran as a batch — the commit history goes F6 → F8 → F9 →
> deploy with no F6.5 execution in `status.md`. They are re-scheduled as F9.5 items
> 2 (FPS), 4 (recent-colors), and 9 (island placement retest). This section is kept
> for the investigation notes it documents; do not execute it separately.

**Goal**: clear the still-open items in `known_bugs.md` before F7 locks in the
submitted build. Time-box this hard — per the existing rule ("P1/P2 are cut, never
F1–F7"), the same applies here: if any item below risks slipping F7 past
2026-07-11, cut it and note it as a known limitation in `WORK.md` instead of blocking
submission on it.

Already resolved (code inspected, both present as of the F6 commit) — check these off
in `known_bugs.md`, no action needed: merge-toast identity leak (`player_label` in
`client/src/main.rs` never falls back to the identity hex, only a name or "another
player" — decision 11); token paste via Ctrl+V/Cmd+V in the import field
(`client/src/ui.rs` around the `import_focused` block); `reset_account` now rolls a
fresh hue via `ctx.rng().gen_range(0..360)` instead of the deterministic per-identity
`start_hue` (`server/src/lib.rs`, `reset_account`).

Tasks:
1. **Recent-colors seeding**: the starting hue (added to `inventory` at
   `client_connected`, `obtained_with: None`) never enters `last3` — a fresh player's
   swatch row is empty even though they're actively painting with that hue. `last3` is
   currently only pushed on an explicit swatch click or a merge-toast nudge
   (`client/src/ui.rs`). Seed it with the current brush hue on the first frame after
   connecting, in both clients.
2. **Island placement**: author-reported "ilots aren't correctly placed in the world."
   Investigated here: `slot_coords`/`slot_center`/`cube_round`/`axial_to_world` in
   `client/src/world.rs` are bit-for-bit identical to `server/src/lib.rs`'s `geometry`
   module today, and `SLOT_SPACING`(29) vs `ISLAND_RADIUS`(13) leaves the intended ~3-tile
   gap with no overlap — geometry-formula drift is ruled out as of the current code. The
   report predates the F2/F3/F4/F6 client rewrites, so it may already be stale; re-test
   fresh with 3+ islands before investigating further. If it still reproduces, look at
   render-only concerns next (culling pop-in at `main.rs`'s `in_view` padding, or a
   stale/duplicate subscription row) rather than the shared geometry math.
3. **FPS at scale** (40 fps @ 10 islands, 30 fps @ 18 — author's ceiling for the jam):
   root cause found by inspection, not yet fixed. Both clients rebuild a full
   `HashMap` from *every* `island_cell` row in the world on *every frame*
   (`client/src/main.rs:593-594`, `client/src/bin/web.rs:958-959`), regardless of view
   culling — cost scales linearly with total painted cells across all islands (547/island),
   which matches the reported degradation shape exactly. Fix: only rebuild from cells
   whose island is `in_view` (view culling already exists and runs first, at
   `main.rs:634-640` / the web equivalent — the cell-color map just isn't using it yet),
   or maintain the map incrementally from the subscription's insert/update/delete
   events instead of a per-frame full collect. Apply the same fix to both clients so web
   (the judged target) actually benefits, not just native (what the author hand-tests).

Files: `client/src/main.rs`, `client/src/bin/web.rs`, `client/src/ui.rs`. Server:
read-only unless task 2's re-test finds a genuine server-side placement bug.

Verify: author hand-tests each on the native client per the existing protocol (F2's
ground rule — don't use the raylib GUI as an automated smoke test, but this is exactly
the kind of change that needs a human looking at the screen); record
VERIFIED/REASONED in `status.md`; re-check `du`/build still clean. Check off the
corresponding line in `known_bugs.md` as each lands.

## F7 — Deployment + itch.io submission package — DONE (2026-07-11)

> **2026-07-11 status check**: F7 is complete — the game is deployed and submittable.
> F8/F9 already shipped ahead of it and remain live. Remaining time before the deadline
> (2026-07-12 18:00 UTC) is free for further P1/P2 work or the F6.5-adjacent backlog,
> subject to the freeze window rule below.

**Goal**: publicly playable from itch.io before the deadline. DO THIS THE MOMENT F5/F6
LAND — it de-risks the wss/itch path; everything after is redeploys.

Tasks:
1. Host resolution rule in the web client (`game.html`): if `location.hostname` is
   `localhost`/`127.0.0.1`/a private-range IP → `ws://<hostname>:3000`; otherwise →
   `wss://spacetime.mister-esman.uk`. (itch.io serves from `*.itch.zone`, so
   hostname-based resolution to the game server cannot work there.)
2. VPS: `git pull`, publish the module (the VPS runs SpacetimeDB 2.7 — module is
   compiled by the local pinned 2.6.1 CLI and published remotely, same as before;
   verify server-side version acceptance), `./build-web.sh` on the VPS, sanity-check
   `https://raylib.mister-esman.uk`. Stop the old bots
   (`hexmerge-bot@*.service`) — they speak the old schema; adapting them is P2.
3. itch.io package: a zip whose root has `index.html` = the SINGLE-game page (adapt
   from `game.html`; the dual-iframe wrapper is NOT the submission page) + wasm/js/data
   files. itch HTML5 settings: viewport exactly 720×720, enable the fullscreen button,
   set the mobile-friendly flag. Upload as a DRAFT project first and test end-to-end
   from itch.io itself (wss through Cloudflare, touch on a phone via the itch page).
   The itch "Run game" button cannot be redirected (and the jam requires the embedded
   wasm to be evaluated on itch anyway) — instead add an "also playable at
   https://raylib.mister-esman.uk" link in the project page description; both entry
   points hit the same server, so it's one shared world either way.
4. Keep the module + Caddy + VPS up through the voting window (ends 2026-07-18); note
   the ops commands in `WORK.md`.

Files: modify `client/web/game.html` (+ a small `client/web/itch/` staging dir or a
zip script), `WORK.md`. Do NOT touch: game logic.

Verify:
- From the itch draft page (not LAN): two devices see each other, paint + merge work.
- Reload the itch draft page → same identity (token in `localStorage` survives inside
  the itch iframe); total user count unchanged in
  `spacetime sql hexmerge "SELECT COUNT(*) FROM user"`.
- `du -b` of the zip < 64 MB. 720×720 exactly, no scrollbars.
- **This feature done = the game is submittable. Submit early; re-upload improved
  builds any time before 2026-07-12 18:00 UTC.**

---

## Freeze window rule (author decision, 2026-07-11)

NOTHING ships after the submission deadline — not even server-side. From
2026-07-12 18:00 UTC until voting ends (2026-07-18 18:00 UTC), the module, the
standalone site, and the itch build are all frozen: the game people rate is exactly
the game that was submitted. Consequences:
- P1/P2 features count only if they are deployed AND hand-tested before the deadline;
  otherwise they wait until after voting ends.
- During the window, ops is watch-only: keep VPS/Caddy/SpacetimeDB up (uptime is not
  an update). Sole exception, at the author's explicit call: emergency service
  restoration (server down, crash loop, actively abused exploit) — restore service,
  change no gameplay.
- After voting ends, updates resume freely (itch re-uploads become possible again);
  redeploy server + site + itch build together, so schema compatibility with the old
  frozen client never becomes a constraint.

# P1 — after the game is submittable

## F8 — Likes, island info, 5-minute re-ranking

- `island_like` table (unique (island, liker)); like button in an island-info popup
  (click/tap a foreign island's center area or an "info" mode): creator name, likes,
  created date, link. `XP_LIKE` to the owner, once per liker per island.
- Scheduled reducer chain: at `T - 5 s` set `config.next_rerank_at` (clients render a
  countdown banner from it); at `T` re-sort islands by likes desc (admin island pinned
  at slot 0, ties by `created_at`), rewrite `island.slot`, schedule the next cycle.
  Cells store island-relative coords, so moving an island = updating one row — margin
  art never moves.
- Verify: `spacetime sql` shows slots permuted after forcing a short period in a test
  publish; countdown appears in both clients; island art rendered at the new slot.

## F9 — Links + XP economy

- `set_island_link(rate_id: u32)`; label on the island info popup; click → open
  `https://itch.io/jam/raylib-6x-gamejam/rate/<id>` (native `OpenURL`, web
  `window.open`) and `click_link` reducer → `XP_LINK_CLICK` to the owner (dedupe: one
  credit per clicker per island).
- Time XP: scheduled reducer every 60 s grants 1 XP to users with fresh `last_seen`.
- Level-up feedback: toast + sat-cap slider max visibly grows.

## F9.5 — Post-deploy bug sweep (author-ordered: do before F9.6 and F10)

**Goal**: fix everything the author hit while hand-testing the DEPLOYED build
(2026-07-11), plus the F6.5 leftovers that never ran. Everything here must be deployed
AND author-hand-tested before the freeze (2026-07-12 18:00 UTC). The list is
priority-ordered — work top-down, cut from the bottom; anything cut goes back to the
backlog and waits for the post-voting window.

Tasks (priority order):

1. **Account import is broken (CRITICAL)** — pasting a token creates a NEW account
   instead of recovering the existing one. Author reproduced it EVERYWHERE: standalone
   site, itch embed, cross-origin (copy on one, import on the other), and local dev —
   so the import flow itself is broken, not origin-partitioned storage. The plumbing
   on record: `ui.rs` import field → `web.rs` `actions.import_token` (~line 769) →
   JS `window.stdb.importToken(token)` → `localStorage.setItem(TOKEN_KEY, ...)` +
   reload → connect URL appends `?token=...` (`game.html` ~lines 100–107). Suspects,
   in order: (a) does `importToken` actually reload the page in the deployed build
   (F6 spec said "page reload is acceptable" — verify it happens at all); (b) the
   `failedBeforeOpen >= 2` purge (~lines 139–140) deleting a VALID imported token when
   the socket bounces (e.g. wss retries through Cloudflare), then minting a fresh
   identity; (c) whether SpacetimeDB 2.6.1 accepts `?token=` on `v1.json.spacetimedb`
   at all — F6's verify was recorded REASONED, never run live, so the whole path may
   never have worked; probe it the way F5 probed the wire format; (d) TOKEN_KEY slot
   namespacing (`stdb_token_slot<N>` vs `stdb_token`, `game.html` ~line 90) differing
   between the itch `index.html` page and `game.html`. Native import is secondary —
   web is the judged target.
   Verify: copy ID from account A → import in a private window → same short identity
   in the header, `spacetime sql hexmerge "SELECT COUNT(*) FROM user"` unchanged;
   repeat itch ↔ standalone in both directions.
2. **FPS at scale** (ex-F6.5 task 3, root-caused there, still unfixed): both clients
   rebuild a full `HashMap` from EVERY `island_cell` row in the world on EVERY frame
   (`main.rs` ~:593, `web.rs` ~:958 at F6.5 time — re-locate after the F8/F9 drift),
   cost linear in total painted cells regardless of view culling. Fix in BOTH clients:
   build the map only from islands that are `in_view`, or maintain it incrementally
   from the subscription's insert/update/delete events. Target: the author's
   40 fps @ 10 islands / 30 fps @ 18 goes back to a steady 60.
3. **Merge range too short** — `MERGE_DIST` 1.0 → 2.0 (constants table updated;
   `server/src/lib.rs` constants ~line 9 is the live site — mirror wherever clients
   re-declare it). Author tunes by feel during hand-test; write the settled value back
   into the constants table.
4. **Recent-colors: seeding + reset** (ex-F6.5 task 1 + new report "reset doesn't
   reset the last 3"): seed `last3` with the current brush hue on the first frame
   after connect AND after a successful `reset_account` (clear, then reseed with the
   fresh hue). Both clients; same code area, do together.
5. **Modal click-through** — clicking the color-overlay close button also
   paints/clicks the tile behind it, violating F3's "overlay is modal" rule. Swallow
   pointer input consumed by overlay chrome, both clients. (Listed in
   `other_ideas.md` but it's a bug against spec, so it rides in the sweep.)
6. **Cursor size vs zoom** — other players' cursors are drawn screen-space today, so
   zooming out leaves them huge relative to tiles. Draw them in world units
   (≈ tile-sized, per the author's note in `other_ideas.md`), clamped to a minimum
   on-screen size so they stay findable when zoomed far out. Both clients. Supersedes
   the backlog "cursor scales with zoom" entry.
7. **Island info on hover** (decision 17): popup opens on hover over a foreign island
   (~200 ms delay so paint sweeps don't flicker it; suppressed while a drag/paint
   stroke is active), closes on hover-out. Double-click-to-like unchanged; touch
   unchanged (tap opens, double-tap likes).
8. **Safari (macOS) zoom too fast/jumpy** — trackpad scroll zooms in huge
   uncontrollable steps. Likely cause: Safari's wheel deltas differ in scale/deltaMode
   from Chrome/Firefox and raylib-emscripten consumes them raw. Fix at the JS layer of
   `game.html`: intercept `wheel`, normalize by `deltaMode`, clamp the per-event zoom
   step (sign + capped magnitude); check Safari's proprietary
   `gesturestart`/`gesturechange` pinch events while there. Verify: author's Mac only
   — and judges plausibly rate on Macs, so don't cut this one lightly.
9. **Island placement retest** (ex-F6.5 task 2): the report predates the F2–F6
   rewrites and geometry drift was ruled out by inspection. Retest fresh on the
   deployed build with 3+ islands; ONLY if it still reproduces, chase render-side
   causes (culling pop-in padding, stale/duplicate subscription rows). If it doesn't
   reproduce, check it off and move on.
10. **Dead-player reap** (promoted from the backlog): scheduled reducer (piggyback the
    F9 time-XP cadence or its own 60 s schedule) deletes `user` + `island` (+ that
    island's `island_like` / link-click rows + the user's `inventory`) when
    `online == false`, `last_seen` older than 5 min, the island has zero
    `island_cell` rows, AND inventory has ≤ 1 row (the seed hue — XP is NOT part of
    the test, time-XP may have granted a few). Accepted edge case: a reset veteran
    idling 5 min with a still-empty island gets reaped — jam-acceptable, note it in
    `status.md`. Frees the slot and deflates the drive-by player count. Server
    change ⇒ full publish + regen + web-mirror cycle per ground rules.

Files: `client/src/main.rs`, `client/src/bin/web.rs`, `client/src/ui.rs`,
`client/web/game.html`, `server/src/lib.rs` (items 3, 10, and 1 if server-side),
generated bindings. Redeploy per F7's runbook after each server publish; keep
`./build-web.sh` green at every item boundary.

Verify: author hand-tests each item ON THE DEPLOYED BUILD (that's where the bugs were
found, not LAN); `status.md` VERIFIED/REASONED per item; check off each
`known_bugs.md` line as it lands; `du -sh client/web` still well under 64 MB. Also
check off in `known_bugs.md`, no code needed (resolved back in F6 per F6.5's notes):
merge-toast identity leak, paste-in-import-field, reset-random-color.

## F9.6 — UX polish batch (author-ordered: before F10 — "polish is judged, admin can slip")

**Goal**: quick player-facing wins triaged from `other_ideas.md`. Same freeze
constraint as F9.5; cut freely from the bottom. Items 1–2 implement decisions 18–19.

Tasks:
1. **Eraser** (decision 18): reducers `erase_island_cell(q_local, r_local)` /
   `erase_margin_cell(q, r)` — delete the cell row; same validation as the matching
   paint reducer (ownership/margin membership, token bucket charge). Client: eraser
   toggle in the footer (+ `E` key), cursor visibly shows eraser mode. Server change
   ⇒ publish + regen + web mirror.
2. **Middle-click eyedropper** (decision 19): picks the hovered tile's h/s/v — hue
   must already be in the caller's inventory (client-side check against the local
   inventory subscription; `set_brush` re-validates server-side anyway), sat clamped
   to `SAT_CAP(level)`. If the hue isn't unlocked: toast "not unlocked — long-press
   to merge". Mouse only; no touch equivalent.
3. **Heart icon for like** — replace the like button/label glyph in the island popup
   with a heart (filled = already liked), both clients.
4. **Color overlay ergonomics**: Escape closes it; clicking outside the panel closes
   it; swatches sorted by hue; hovering a swatch shows its hex code (of the swatch as
   rendered).
5. **Keybindings/help overlay**: Escape with no overlay open toggles a minimal
   controls list (this is the minimal version of F12's help-overlay item — F12 then
   only extends it). Escape with an overlay open closes that overlay (consistent with
   item 4).
6. **Keyboard + right-drag camera**: arrow keys AND WASD pan; Q/E zoom out/in (wheel
   behavior untouched); right-click-drag pans (in addition to SHIFT+drag /
   middle-drag). Text fields (name, import) must swallow keys while focused so typing
   a name doesn't pan the camera.
7. **Launch intro**: on the first world render after connect, the camera starts
   framing the whole occupied world, then eases to the player's island over ~1.5–2 s;
   any input skips it. Client-only, both clients.
8. **Borderless far zoom**: when the on-screen hex size drops below a threshold
   (executor picks, ~4–6 px), skip the tile outline pass so the world reads as a
   painting (author's note) — also a small render win. Both clients.

Files: `client/src/main.rs`, `client/src/bin/web.rs`, `client/src/ui.rs`,
`client/src/world.rs`, `client/web/game.html`; `server/src/lib.rs` + bindings for
item 1 only.

Verify: author hand-test on the deployed build; `./build-web.sh` green; `du` check.
Eraser specifically: erase own island cell → row gone
(`spacetime sql hexmerge "SELECT COUNT(*) FROM island_cell"` drops), erase a FOREIGN
island cell → rejected; eyedropper on an un-unlocked hue → toast, brush unchanged.

Not scheduled from `other_ideas.md` (stays in the backlog below): "Merge with me!"
center bot, customizable island border color.

# P2 — only if time remains before 2026-07-12 17:00 UTC

- **F10 Admin**: SHIPPED (2026-07-11). `claim_admin(password)` verified against a
  SHA-256 constant (repo is public — never a plaintext password), grants
  `config.admin`, idempotent for the current admin; relocates the caller's existing
  island (everyone gets one at first connect) to the reserved slot 0, swapping with
  whatever island currently holds it rather than deleting anything, via the same
  bump-through-a-temporary-slot technique F8's `rerank_fire` uses for `slot`'s
  `#[unique]` constraint. `set_frozen(frozen)` (admin-only) toggles `config.frozen`,
  enforced by a new `check_not_frozen` guard at the top of all 17 player-facing
  mutating reducers (scheduled/system reducers and the three admin reducers
  themselves are exempt — freeze stops players, not the world's background clocks
  or the admin's own tools). `delete_island_cells(island_id)` (admin-only) wipes an
  island's painted cells for moderation, leaving the island row itself untouched.
  Backups documented as `spacetime sql` dump-per-table commands in `WORK.md`'s new
  Admin/Backups sections (manual restore-by-hand, no automated snapshot/replay).
  No client UI in either target — CLI-only ops tooling, per this bullet's own
  "admin tooling is minimal for now" framing. Verified live end-to-end via a
  throwaway WebSocket probe + `spacetime sql` against the local instance (wrong
  password rejected, correct password grants admin + relocates the island incl. the
  swap-with-a-previous-admin path, non-admin calls to `set_frozen`/
  `delete_island_cells` rejected, freeze actually blocks `paint_island_cell` and
  unfreeze restores it, `delete_island_cells` wipes exactly the targeted island).
  NOT yet published to the production VPS — that publish is a separate, explicit
  step for the author to trigger (same access gap as the border-customization
  feature above).
- **F11 Flying gift**: scheduled spawn of a drifting pickup (position table row,
  client-animated), click/tap to claim → random hue or XP. Design fleshed out by the
  executor (plan.md only had the one-liner above) — see status.md's F11 batch note for
  the full rationale; summary:
  - *Schema*: `Gift(id, x, y, spawned_at, expires_at)`, public. At most one active at a
    time (simplest P2 scope call) — a repeating `gift_tick` (every
    `GIFT_SPAWN_PERIOD_SECS`) sweeps expired rows then spawns a fresh one (uniform-in-
    disk over the currently-occupied world bound, same `occupied_rings`/`SLOT_SPACING`
    math `paint_margin_cell` uses) if none remain. `GIFT_LIFETIME_SECS` < the tick
    period so there's visible down-time between gifts.
  - *Drift*: `x`/`y` are the spawn center; the actual position drifts in a small circle
    around it, purely a function of elapsed time since `spawned_at`
    (`GIFT_DRIFT_RADIUS`/`GIFT_DRIFT_PERIOD_SECS`, shared constants) — both clients
    render it identically with no continuous position sync, same trick `next_rerank_at`
    uses for the countdown banner.
  - *Claim*: `claim_gift(gift_id)` — caller must be within `GIFT_CLAIM_DIST` (shared
    constant, world units) of the gift's CURRENT drifted position (not just its spawn
    point), checked server-side against the caller's last-reported `set_pos` cursor —
    same anti-cheat posture as cursor-merge. Reward is a coin flip: a fresh random hue
    (retried up to 8 rolls against one already owned, falling back to XP if all 8
    collide) or a flat `XP_GIFT`. `Inventory` gets a new `from_gift: bool` field
    (appended at the end, same reason `Island.border_color` was) so a gift-granted hue
    doesn't get misread as `reset_account`'s reseed by the client's existing
    `obtained_with.is_none()` check.
  - *Rendering*: world-space pulsing "box + ribbon" icon (`world::draw_gift_icon`),
    click/tap resolved on PRESS (not release) and consumes the whole gesture so the
    same click can't also start a paint stroke or long-press underneath it — mirrors
    the existing `suppress_map_until_release` latch.
- **F12 Polish**: sounds (raylib `LoadSound`, CC0 assets only), bots adapted to the new
  schema (they keep the world alive for raters — include the backlog's "Merge with me!"
  center bot here), help overlay explaining merge (extends F9.6's minimal keybindings
  overlay), page styling on itch. IMPLEMENTED (2026-07-11), all four sub-parts — see
  status.md's F12 batch notes for the executor's design calls on each:
  - Bots: `client/src/bin/bot.rs` converted from the pre-F2 fixed-canvas pixel space to
    world-cartesian coordinates (`PATH_RADIUS` now clears the admin island's ~26-unit
    reach); new third `center` bot idles at the world origin with display name literally
    `"Merge with me!"`.
  - New: cursor name labels (`world::draw_cursor_label`, both clients) — the render
    surface the center bot's callout needed; a general small-text-above-cursor label for
    every online player, gated on zoom so it doesn't clutter zoomed out, defensively
    truncated since `set_name` has no server-side length cap.
  - Help overlay (`ui::draw_help_overlay`) now has a "Merging colors" section above the
    existing keybindings list, explaining the actual theme mechanic in-game for the
    first time.
  - Sounds: `client/src/sfx.rs` (new shared module), 4 CC0 rFXGen sfx from raylib's own
    bundled examples, embedded via `include_bytes!` (not file-path `LoadSound`, so the
    web target needs no emscripten `--preload-file` staging), wired to the existing
    merge/gift/levelup/"not unlocked" toast events. Audio device init treated as
    fallible (`Option`), degrades to silence rather than crashing if unavailable.
  - itch page styling: copy drafted in `itch-page.md` (new, repo root) — publishing it
    needs the author's itch.io login, which the executor doesn't have.
  NOT committed — pending the author's hand-test pass (including actually hearing the
  sounds), same convention as every prior batch.
- **F13 Hexa event** — the merge mechanic at 6 (author-designed). IMPLEMENTED
  (2026-07-11) — see status.md's F13 batch note for the executor's design calls;
  summary:
  - *Trigger* (server, in `set_pos` after the pairwise-merge scan): count eligible
    cursors — online, `last_seen` < `PRESENCE_TIMEOUT`, not locked — within
    `HEXA_RADIUS` of the caller whose brush hue matches the caller's within
    `HUE_TOLERANCE` (circular distance; exact equality would break with the ±5° hue
    slider). Count includes the caller; at `HEXA_SIZE` (6), ignite. Implemented
    exactly as specified in `set_pos`/`apply_hexa` (`server/src/lib.rs`).
  - *Effect*: the participants' color dictionaries are pooled — every hue owned by any
    participant is granted to every participant missing it (a 6-player hexagon shares
    everything its members know). `XP_HEXA` to each participant, ONCE per player ever.
    Pooling is idempotent for a fixed group (a second ignition grants nothing new), so
    no cooldown is needed. (Author-confirmed: pooling is among the 6 participants
    only — never server-wide.) Executor design call: that same idempotence also gates
    the `hexa_event` log row itself (`apply_hexa`'s `changed` flag) — a cluster that
    stays formed keeps re-matching its own trigger every `set_pos` tick it holds, so
    without the gate every one of those ticks would insert a fresh event row and spam
    the clients' ignition flash; only a pass that actually grants something new logs
    one. No cap on cluster size past `HEXA_SIZE`: a 7+-member same-hue cluster pools
    among all of them, not just the nearest 6 (not a scope the author's "among the 6
    only" ruling addressed — that ruling was about never pooling server-wide, not
    about capping cluster size — so this is the executor's read of it, flag if wrong).
  - *Schema*: per the freeze window rule, F13 ships either before the deadline
    (unlikely) or after voting ends — in both cases server + all clients redeploy
    together, so no compatibility constraint applies. Keep dedicated tables anyway,
    for cleanliness: `hexa_reward(identity pk, at)` = who already received the
    one-time XP (server-internal, NOT `public` — clients only ever observe its effect
    through `User.xp`/`Inventory`, same as every other XP grant); `hexa_event(id
    auto_inc, at, cx, cy, member_count)` for the ignition animation (`public`).
    Granted inventory rows use `obtained_with = None` + a `hexa_event` timestamp join
    for the special "obtained in a Hexa" mention (both stamped with the same
    `ctx.timestamp` in the same reducer call, so the join is an exact match, not a
    tolerance window). Author follow-up, same day: also `hexa_cluster(identity pk,
    cluster_id, cx, cy, member_count, vertex_index, ignited)`, `public` — see
    *Rendering* below for why.
  - *Rendering* (client-only; the frozen itch build simply won't show it): cursors
    currently merged (same hue within tolerance, within `HEXA_RADIUS`) are DISPLAYED
    snapped onto the vertices of a regular hexagon around the cluster centroid — real
    network positions are untouched (detection keeps using them); display positions
    lerp to their vertex slot (new client-only `HEXA_SNAP_LERP_SECS`/
    `HEXA_VERTEX_RADIUS` constants in `world.rs`, not shared with the server — it has
    no notion of a vertex layout); vertex assignment is stable (sort members by
    identity). After a pairwise merge your cursor visibly settles beside your
    partner's: two vertices of an incomplete hexagon, waiting for four more. At 6:
    ignition — the hexagon edges render bright/thick instead of a faint preview chain,
    toast, inventory visibly fills.

    **Author follow-up (2026-07-11), superseding the first pass's two rendering
    calls**: (a) the LOCAL player's own cursor must also visibly move to its hexagon
    slot while merging, not stay glued to the mouse; (b) cluster membership should be
    server-authoritative — "the server would tell that there is an HEXA happening and
    give an id and position to players so that everyone sees they are merging" — not
    each client guessing its own approximate clustering. Reworked accordingly:
    `set_pos`'s existing per-caller detection scan (used for the ignition trigger
    either way) now ALSO upserts a `hexa_cluster` row for EVERY detected member (not
    just the caller) whenever the caller's own cluster has >= 2 members — `cluster_id`
    is a hash of the sorted member identities (`hexa_cluster_id`), not an arbitrary
    counter, so independent callers' own scans naturally agree on it without
    cross-call synchronization, and it changes the instant membership actually
    changes; `vertex_index` (assigned from that same sorted order) is which hexagon
    slot a member renders at, so clients no longer derive vertex assignment
    themselves — they just read `hexagon_vertex_positions(centroid, member_count)
    [vertex_index]`. A caller whose own cluster drops below 2 deletes their own row.
    Since a departing/disconnecting member might never call `set_pos` again to clear
    their row, a new repeating `hexa_sweep` tick (`HEXA_SWEEP_PERIOD_SECS`, 2s,
    private `hexa_sweep_schedule` table) deletes any `hexa_cluster` row whose owner
    has gone offline or stale. Both clients dropped their own client-side clustering
    algorithm entirely (`world::hexa_clusters`/`HexaCandidate` removed) — they group
    `hexa_cluster` rows by `cluster_id` and render directly from server truth,
    including their OWN identity's row for the local-cursor override (painting/hover
    logic still reads the real mouse position; only the cursor DRAW call moves).
    "Flash" is still a sustained bright/thick hexagon while `ignited` holds, not a
    timed pulse, and the ignition toast still reuses the existing merge sfx cue
    (neither of those two calls changed).
  - *Verify*: 6 clients (native instances + web iframes + adapted bots) with distinct
    hues converge → pairwise merges cascade, snap rendering forms the hexagon, at 6
    every participant's inventory becomes the union (`spacetime sql`: identical hue
    sets per participant), XP granted exactly once (re-form the hexagon → no new XP,
    `hexa_reward` row count unchanged). Server-side VERIFIED live (not hand-tested in
    the GUI, per the author's own testing protocol) via a throwaway 6-identity SDK
    probe (same precedent as F11's throwaway WebSocket probe, deleted after use): 6
    distinct identities homogenized to the exact same live brush hue via a legitimate
    tile-merge/eyedropper off one shared painted cell (so the same-hue detection check
    passes without needing 6 colliding random seed hues), clustered at the world
    origin, `set_pos` fired in sequence — the 6th call's own trigger scan saw all 6
    fresh + same-hue + close and ignited. Confirmed via `spacetime sql`: exactly 6
    `hexa_reward` rows (one per identity) and exactly 1 `hexa_event` row after
    ignition; each participant's inventory grew to the union of the 6 distinct seed
    hues plus the shared eyedropped hue (6 rows each); XP matched `XP_MERGE_NEW` (from
    the eyedropper) + `XP_HEXA` exactly. Re-triggered `set_pos` on the still-formed
    cluster afterward: `hexa_reward` count and every participant's `xp` were BOTH
    unchanged, and `hexa_event` stayed at exactly 1 row — confirms the idempotence
    gate. Client rendering (both native `main.rs` and web `bin/web.rs`, plus
    `game.html`'s subscription list) REASONED, not hand-tested — `cargo build` (server,
    native client, bot) and `./build-web.sh` all clean; the author drives actual GUI
    testing per the established protocol.

    **Follow-up verify (same day, after the server-authoritative rework above)**:
    re-ran a similar 6-identity probe against `hexa_cluster` specifically —
    clustering all 6 confirmed identical `cluster_id` and stable, distinct
    `vertex_index` values (0..5) across every member's own row, `ignited: true` on
    all 6 exactly once `member_count` hit 6. Walking one identity far away and
    nudging a remaining member confirmed: the departed identity's own row was
    deleted immediately, the other 5's rows refreshed to `member_count: 5`,
    `ignited: false`, and a NEW `cluster_id` (membership genuinely changed, so the
    hash changed too) — confirms live shrink-on-departure, not just grow-on-join.
    Leaving the group idle (no further `set_pos` from anyone) for ~6s confirmed the
    `hexa_sweep` safety net clears every remaining row once `last_seen` goes stale —
    `spacetime sql`'s `hexa_cluster` table was empty afterward. Client rendering
    (the local-cursor snap override, dropping `world::hexa_clusters` in favor of
    reading `hexa_cluster` rows directly) again REASONED via clean builds, not
    hand-tested in the GUI.

- **F14 Community island (playground)** — author ruling 2026-07-11 (see decision 20),
  resolving the backlog's "center-island identity" question below. Slot 0 becomes a
  permanent, ownerless canvas anyone can paint or erase, ending its dependence on
  `claim_admin`:
  - *Schema*: none. Reuses the existing `island`/`island_cell` tables — the community
    canvas is just the `Island` row at `slot == 0` with `owner == Identity::ZERO`
    (a real, never-issued sentinel value; native client code compares it directly,
    web's hand-parsed hex path compares against its 64-zero hex string).
  - *Server*: `client_connected` lazy-seeds the slot-0 `Island` row if missing (same
    pattern already used there for `config`/the four scheduled-reducer rows — a
    `--delete-data=never` republish of the live database doesn't re-run `init`, so
    lazy-seeding on connect is the only way an existing database picks up the row).
    Two new reducers, `paint_community_cell`/`erase_community_cell(q_local, r_local)`:
    same bound check (`hexdist <= ISLAND_RADIUS` from slot 0) and same paint-token
    charge as `paint_island_cell`/`erase_island_cell`, but with NO ownership check —
    any connected caller may write. `claim_admin` had its island-relocation block
    removed entirely (it only ever swapped the claimant's OWN island into slot 0;
    with slot 0 permanently occupied by the community island, that swap no longer
    makes sense) — it now only verifies the password and sets `config.admin`.
  - *Client* (both native `main.rs` and web `bin/web.rs`, mirrored): the per-cell
    `classify`/paintability check gets a third case — inside slot 0's territory but
    not matching "own island" or "margin" — dispatched to
    `paint_community_cell`/`erase_community_cell` exactly like the existing two paint
    kinds (extends the `(kind: u8, q, r)` dispatch with a new `kind = 2`). Its
    unpainted tiles render WHITE (`Color::new(255,255,255,255)`) instead of the usual
    gray placeholder fill, in both clients — a visual tell that this is the shared
    canvas, at a glance, before a single pixel's been painted (author-requested,
    2026-07-11); shared as `world::unpainted_island_fill`. Long-press tile-merge and
    the middle-click eyedropper are UNCHANGED (both already operate generically on
    any painted cell via `island_at`, which the community island's real `Island` row
    satisfies for free) — so merging with a stranger's community-canvas brushstroke,
    or eyedropping its color, both work exactly like on any other island.
  - **Author reversal (2026-07-12, pre-submission session)**: hover-tooltip,
    single-click/double-click info popup, and like/unlike are now DISABLED on the
    community island — its sentinel owner (`Identity::ZERO` / `COMMUNITY_OWNER_HEX`)
    otherwise passes every `owner != me` foreign-island filter like a real player's
    island. Both clients' `info_target` and `currently_hovered_foreign` filters gained
    an explicit sentinel-owner exclusion; server-side `like_island`/`unlike_island`
    reject `owner == Identity::ZERO` as a backstop against raw reducer calls.
    `player_label`'s "Free Isle" naming stays (still used by the merge/eyedrop toast
    path), it just no longer has a popup to render inside.
  - **Web zero-identity bug fix (2026-07-12)**: the wire encodes `Identity` as
    *minimal* hex (`Identity::ZERO` arrives as `"0x0"`, not 64 zeros), so web's old
    `normalize_identity` (strip `0x` + lowercase only) never produced a value equal to
    `COMMUNITY_OWNER_HEX` — silently breaking the white unpainted-fill and the "Free
    Isle" label on web only (native compares typed `Identity` values, unaffected).
    Fixed by moving normalization into shared `world::normalize_identity_hex`, which
    also left-pads to 64 hex chars; web's `normalize_identity` now delegates to it.
  - *Verify*: `spacetime sql hexmerge "SELECT id, owner, slot FROM island WHERE slot
    = 0"` shows the sentinel row after one `client_connected`; `paint_community_cell`/
    `erase_community_cell` succeed for an identity that owns no island there and fail
    outside the radius bound; painting/erasing on one's own island and the margins is
    unaffected; `claim_admin` no longer moves any island (`island.slot` for the
    claimant's own island is unchanged after claiming); the community island's
    unpainted tiles render white (distinct from every other island's gray
    placeholder) in both clients; hovering/clicking/double-clicking it opens NO
    popup and registers NO like in either client, and a raw `like_island`/
    `unlike_island` reducer call on it is rejected server-side; `./build-web.sh` and
    `cargo build` (server, both clients, bot) stay clean.

- **F15 Title screen ("hexel" wordmark + Draw button)** — author chat request,
  2026-07-12 (submission day): "Create a nice hexagon typography for hexel"
  (lowercase `h` explicitly requested), then "a title screen with the title in big,
  an animated button 'Draw' and a semi transparent background, showing a hint of
  the whole map behind." Client-only, no schema/server change, no republish needed.
  - *Wordmark*: the word is spelled out of hexagon cells on the game's own flat-top
    axial grid (same `1.5q / sqrt3*(r + q/2)` geometry and `world::draw_hex`
    renderer as the map itself — odd columns sit half a row lower, which gives the
    `h`'s shoulder and the `e`'s caps their rounding for free). Glyphs are const
    cell tables in `ui.rs` (`TITLE_GLYPH_*`: cells as `(q, 2*v)` with `v = r + q/2`
    the visual row; doubling keeps the half-row offsets integral). The `x` is a
    quincunx (two arms top/bottom + the crossing cell), the `e` a ring with its
    mouth open bottom-right, ascenders 7 cells tall. Cell hue sweeps 0->330 across
    the word's 17 columns at sat 70 / val ~88 via `world::hsv_color` (the game's
    canonical HSV path) with a slow brightness shimmer traveling along the word —
    the wordmark IS the hue-merge continuum. Cell radius 23 px -> just under 600 px
    wide on the 720 px screen, plus a soft per-cell drop shadow (own pass, so
    shadows never land on neighboring fills). Glyph shapes were iterated visually
    offline (PIL renders) before porting the final cell tables to Rust.
  - *Title screen*: new `UiState.title_active`, starts `true` every launch. While
    set, `ui::draw` renders — INSTEAD of the HUD — a full-screen `(8,9,14,205)`
    backdrop (the map stays hinted behind), the wordmark centered at y=250, and a
    rounded "Draw" button (pencil icon + label, `measure_text` width cached via the
    same `OnceLock` warm-from-`handle_input` trick as the Colors label) that
    breathes (±3% scale sine) when idle and settles slightly enlarged on hover.
    `ui::handle_input` swallows all HUD input while the title is up; clicking the
    button (or Enter) clears the flag. `UiState::any_modal_open()` now includes
    `title_active`, which gates — through the existing chokepoints in BOTH clients
    (`map_input_allowed`, `suppress_map_until_release`, the hover filters) —
    painting, panning, gift claims, hover popups, and the `set_pos` cursor
    heartbeat (no ghost cursor broadcast while sitting on the title). Both clients'
    `over_map_area` also gains a `!title_active` term: the title covers the whole
    screen, and the term is computed before `handle_input` flips the flag, so the
    dismissing click can never paint the tile under the button.
  - *Camera*: while the title is up, both clients hold the camera each frame on the
    `world_fit` whole-occupied-world pose (recomputed as islands stream in) — the
    "hint of the whole map" behind the backdrop. Dismissing the title starts the
    existing F9.6 launch intro unchanged; its `intro_from` calls the same
    `world_fit`, so the ease into the player's own island continues seamlessly
    from the exact pose the title was holding.
  - *Verify*: `cargo check -p server`, `cargo build -p client --bin client --bin
    bot`, `cargo test -p client --bin client`, `cargo test -p server`, and
    `./build-web.sh` all clean. Wordmark + full-screen layout previewed offline
    with the exact layout constants (PIL mock); the live GUI look (shimmer, pulse,
    backdrop alpha over a real map) NOT hand-tested — author drives runtime
    testing, per this repo's protocol.

---

# Backlog — ideas not yet scheduled

Raw notes live in `other_ideas.md` and `known_bugs.md`; triaged here so plan.md stays
the single source of truth. None of these are P0/P1 — pick up only after F7 is
submitted, and only if the freeze window rule still allows a redeploy.

- **Dead-player cleanup** — SCHEDULED: promoted to F9.5 item 10 (2026-07-11), with
  the "no action taken" definition pinned there (empty island AND inventory ≤ 1 row).
- **Center-island identity** ("bot drawing R and a heart" / "should also be a
  battlefield"): RESOLVED and SCHEDULED as F14 (2026-07-11, decision 20) — the author
  ruling was "the middle island should be a playground where anyone can draw
  anything." `heart-bot`/`hexagon-bot`/the F12 "Merge with me!" center bot already
  drew near the world center as always-on players; F14 makes the actual slot-0 island
  itself a shared, ownerless canvas rather than just having bots idle nearby.
- **"Outside is only margin, for big drawings"**: already true by design (the geometry
  spec's margin definition + decision 3) — no action needed, idea already satisfied by
  F1.
- **XP-scaled islands** ("more XP gives more islands?"): would break the fixed
  decision "one island per player" (#10) — a real feature with real scope (multi-slot
  assignment per player, per-island brush switching, new UI). Needs an explicit author
  ruling, not assumed here; do not implement ad hoc.
- **Cursor scales with zoom** — SCHEDULED: promoted to F9.5 item 6 (2026-07-11),
  reframed after deployed-build testing ("dezoom should reduce the size of other's
  cursor"): world-space rendering with a minimum on-screen clamp. Still pairs with
  F13's snap-to-vertex rendering later.
- **Hover-to-see-island-info** — REOPENED and SCHEDULED: the F8 click version did
  turn out to feel wrong in the deployed build, exactly the reopening condition
  reserved here. Now decision 17 + F9.5 item 7 (hover opens, double-click-to-like
  and touch unchanged).
- **"Merge with me!" center bot** (from `other_ideas.md`): a bot cursor idling near
  the world center with a distinct hue and an on-screen callout, so solo raters get
  an easy first merge. Depends on adapting the bots to the new schema (F12) and
  brushes against the center-island identity ruling still pending above — schedule
  as part of F12, not alone.
- **Customizable island border color / transparency** (from `other_ideas.md`):
  SHIPPED (2026-07-11, author override of the note below — deliberately landed
  despite it). `Island.border_color: Option<u32>` (packed HSV, appended after
  `created_at` so `bin/web.rs`'s positional row parsing didn't shift) +
  `Island.border_hidden: bool`. Three reducers: `set_island_border` (pins the border
  to the caller's CURRENT brush color, also un-hides it), `disable_island_border`
  (hides it, `border_color` left untouched), and `show_island_border` (author
  follow-up, 2026-07-11: un-hides WITHOUT touching `border_color` — added once the
  popup UI split "set the color" and "toggle visibility" into two independent
  buttons, so re-showing a border must not silently repin its color). Client-side
  precedence in both `main.rs` and `bin/web.rs`: `border_hidden` → nothing drawn;
  else `border_color` if set; else the pre-existing seed-hue default. UI (author
  follow-up, same date): the old two-button-plus-status-line layout became a single
  "Border: Shown"/"Border: Hidden" toggle (red-tinted while hidden, same language as
  the footer's lock button) plus a "Set border to:" button that previews the pending
  change as `[current border swatch] -> [current cursor swatch]` instead of a bare
  label — `IslandInfo.border_color: Color` (precomputed by the caller, main.rs's
  `resolve_border_color`/web.rs's mirror) and `draw_island_popup` now also takes
  `&HudInfo` for the live brush color. Verified via `spacetime call`/`cargo build`
  against the local instance (not the GUI client, per the author's testing
  protocol): `set_island_border` packs `(hue, sat, val)` correctly,
  `disable_island_border` preserves `border_color` while flipping `border_hidden`,
  re-running `set_island_border` un-hides; `show_island_border` called clean
  (exit 0) against a real owned island. `cargo build -p server`/`-p client --bin
  client` and `./build-web.sh` all clean after the follow-up. NOT yet published to
  the production VPS — that publish is a separate, explicit step for the author to
  trigger. Original deferral note, kept for context: "per-island
  schema field + picker UI — a full publish/regen/mirror cycle for pure cosmetics.
  Post-voting redeploy candidate; not worth a schema cycle before the freeze."
