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

## Canonical constants — single source of truth

Mirror these EXACTLY in: server module, native client, web client (JS + `web.rs`).
Put them in one Rust module per binary (`shared` values re-declared, with a comment
pointing here).

| Constant | Value | Meaning |
|---|---|---|
| `ISLAND_RADIUS` | 13 | hex distance from island center to edge (side 14, 547 cells) |
| `SLOT_SPACING` | 29 | fine-hex distance between adjacent island slot centers (leaves a ~2-tile margin band between islands) |
| `MERGE_DIST` | 1.0 | cursor-merge trigger distance, world units |
| `PAINT_BUCKET_MAX` | 1000.0 | rate limit: bucket capacity (raised 50x from the original 20.0 — felt too restrictive in hand-testing) |
| `PAINT_REFILL_PER_SEC` | 50.0 | rate limit: 1000 tiles / 20 s (raised 50x from the original 1.0, same ratio) |
| `PRESENCE_TIMEOUT` | 3 s | cursor shown / merge-eligible if `last_seen` fresher than this |
| `CURSOR_SEND_HZ` | 20 | max `set_pos` rate, and only when moved |
| `XP_MERGE_NEW` | 25 | XP per newly unlocked hue |
| `XP_LIKE` | 10 | XP to island owner per like (P1) |
| `XP_LINK_CLICK` | 5 | XP to island owner per link click (P1) |
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

## F7 — Deployment + itch.io submission package

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

# P2 — only if time remains before 2026-07-12 17:00 UTC

- **F10 Admin**: `claim_admin(password)` verified against a SHA-256 constant (repo is
  public — never a plaintext password), grants `config.admin`; freeze toggle checked
  by all mutating reducers; delete-island-cells reducer; admin island at slot 0;
  backups documented as `spacetime sql` dump commands in `WORK.md`.
- **F11 Flying gift**: scheduled spawn of a drifting pickup (position table row,
  client-animated), click/tap to claim → random hue or XP.
- **F12 Polish**: sounds (raylib `LoadSound`, CC0 assets only), bots adapted to the new
  schema (they keep the world alive for raters), help overlay explaining merge, page
  styling on itch.
- **F13 Hexa event** — the merge mechanic at 6 (author-designed).
  - *Trigger* (server, in `set_pos` after the pairwise-merge scan): count eligible
    cursors — online, `last_seen` < `PRESENCE_TIMEOUT`, not locked — within
    `HEXA_RADIUS` of the caller whose brush hue matches the caller's within
    `HUE_TOLERANCE` (circular distance; exact equality would break with the ±5° hue
    slider). Count includes the caller; at `HEXA_SIZE` (6), ignite.
  - *Effect*: the participants' color dictionaries are pooled — every hue owned by any
    participant is granted to every participant missing it (a 6-player hexagon shares
    everything its members know). `XP_HEXA` to each participant, ONCE per player ever.
    Pooling is idempotent for a fixed group (a second ignition grants nothing new), so
    no cooldown is needed. (Author-confirmed: pooling is among the 6 participants
    only — never server-wide.)
  - *Schema*: per the freeze window rule, F13 ships either before the deadline
    (unlikely) or after voting ends — in both cases server + all clients redeploy
    together, so no compatibility constraint applies. Keep dedicated tables anyway,
    for cleanliness: `hexa_reward(identity pk, at)` = who already received the
    one-time XP; `hexa_event(id auto_inc, at, cx, cy, member_count)` for the ignition
    animation. Granted inventory rows use `obtained_with = None` + a `hexa_event`
    timestamp join for the special "obtained in a Hexa" mention.
  - *Rendering* (client-only; the frozen itch build simply won't show it): cursors
    currently merged (same hue within tolerance, within `HEXA_RADIUS`) are DISPLAYED
    snapped onto the vertices of a regular hexagon around the cluster centroid — real
    network positions are untouched (detection keeps using them); display positions
    lerp to their vertex slot; vertex assignment is stable (sort members by identity).
    After a pairwise merge your cursor visibly settles beside your partner's: two
    vertices of an incomplete hexagon, waiting for four more. At 6: ignition — flash
    the hexagon edges, toast, inventory visibly fills.
  - *Verify*: 6 clients (native instances + web iframes + adapted bots) with distinct
    hues converge → pairwise merges cascade, snap rendering forms the hexagon, at 6
    every participant's inventory becomes the union (`spacetime sql`: identical hue
    sets per participant), XP granted exactly once (re-form the hexagon → no new XP,
    `hexa_reward` row count unchanged).
