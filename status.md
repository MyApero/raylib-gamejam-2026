# Hexaworld — execution status

Companion to `plan.md`. The executor updates this file after every batch (1–2 tasks).

Evidence tags (mandatory on every checked item):
- `VERIFIED` — ran the command / exercised the behavior and inspected the output.
- `REASONED` — read the code and traced the logic visually.
- `ASSUMED` — unchecked hypothesis; must be verified before the next batch starts.

**Current batch:** F9.5 item 8 — Safari (macOS) trackpad scroll zooms in
huge, uncontrollable steps. The game's zoom step is `wheel_delta * 0.1`
(`client/src/main.rs`/`bin/web.rs`, unchanged), fed by whatever raw
`deltaY` the browser reports — Chrome/Firefox report modest per-tick deltas
for a trackpad scroll, Safari reports much larger raw magnitudes for the
same physical gesture (a known cross-browser wheel-event quirk). Emscripten's
own GLFW3 wheel shim (prebuilt, not part of this repo) only does generic
`deltaMode` scaling, no magnitude clamp.

**Fix, JS layer only (`client/web/game.html`)**: a capture-phase `wheel`
listener on the canvas — capture always runs before Emscripten's own
bubble-phase listener, regardless of script load order — normalizes
`deltaMode` 1/2 (line/page) to an approximate pixel scale, clamps the
resulting magnitude to `MAX_WHEEL_DELTA` (120), and re-dispatches a
synthetic replacement event for Emscripten to consume instead of the raw
one. Guarded against re-triggering itself on its own re-dispatch via
`e.isTrusted` (synthetic/script-dispatched events are always untrusted; real
user input never is). Also added `preventDefault` on Safari's proprietary
`gesturestart`/`gesturechange`/`gestureend` (a genuine trackpad pinch, as
opposed to a two-finger scroll) so it doesn't ALSO zoom the whole browser
page — a no-op on every other browser, which doesn't fire these.

**VERIFIED** mechanically via a scripted Playwright session against the
served `client/web`: real (CDP-injected, trusted) wheel events —
`page.mouse.wheel(0, 3000)` and `(0, -3000)`, standing in for a Safari-sized
trackpad delta — arrive at the canvas re-dispatched at exactly ±120 (the
clamp), while an ordinary `page.mouse.wheel(0, 100)` passes through
completely unchanged (same `deltaY`, still the original trusted event, not
rewritten at all). Screenshotted before/after one huge event: the camera
zooms by roughly one normal step, not a jarring jump. Could **not** verify
against actual Safari (no Mac available in this environment) — this is
REASONED against the documented deltaY-magnitude discrepancy and mechanically
verified for the clamp logic itself, not confirmed against the real browser
it targets. The author hand-testing on an actual Mac is the real
verification here.

---

**Previous batch:** F9.5 item 7 — island info on hover (decision 17), plus one
author-reported bug caught while working the same area: other players'
cursors vanished ~3s after they stopped moving.

**Item 7**: the F8 click-to-open popup "turned out to feel wrong in
practice" per the author's deployed-build testing — decision 17 supersedes
it with hover: popup opens after ~200ms of continuous hover over a foreign
island, closes on hover-out, suppressed while a paint/pan/long-press gesture
is in progress (so a stroke sweeping past a neighboring border doesn't
flicker it open). Double-click-to-like and touch (tap opens, double-tap
likes) are explicitly unchanged.

Implementation, both clients (`main.rs`/`bin/web.rs`): added a NEW,
independent `hover_target: Option<(island_id, Instant)>` tracked purely by
cursor world-position — deliberately NOT touching the existing
`long_press`/`pending_info_click` gesture block at all, which stays exactly
as shipped and still drives double-click-to-like (and, since raylib-web
aliases a single touch to ordinary mouse events, already covers touch tap
too). The two mechanisms can both legally open the same popup; whichever
fires first wins, and re-opening an already-open popup for the same island
is a no-op. Hover-accumulation resets whenever the hovered island changes or
the cursor leaves foreign territory; the actual `open_island_info` call is
gated on `HOVER_OPEN_DELAY` elapsed AND no mouse button currently held (left
or middle — covers paint-drag, pan, and long-press-hold uniformly) AND
neither of the other two overlays being open. A separate, unconditional
check closes `island_popup` the instant the hovered position no longer
matches its `island_id`, regardless of which mechanism opened it.

Considered and rejected: making the popup block `map_input_allowed` the way
the account/inventory overlays do (true modal). That would make it
impossible to double-click-like or long-press-merge the very island whose
popup is showing, since the popup's fixed-position panel (`overlay_rect()`,
unchanged from F8) covers most of the screen and would swallow the click
before it ever reached the map. Left `map_input_allowed` NOT gated by
`island_popup` (as it already wasn't, this predates F9.5) — safe because the
popup only ever appears over FOREIGN territory, where the hovering player
can't paint anyway, so there's no equivalent to item 5's click-through
hazard: any gesture that "leaks through" is by definition a legitimate
foreign-island gesture (merge or like) already.

**Presence bug** (author-caught while testing this batch): other players'
cursors were gated on `is_present(last_seen, now)` — a 3-second freshness
window meant for merge-eligibility (`set_pos`'s cursor-merge check;
`time_xp_tick`), reused for cursor RENDERING too. A player who stops moving
their mouse (reading, picking a color, idle) still has `last_seen` frozen at
their last `set_pos` call (client only sends on movement), so after 3s they
vanished from other players' screens even though fully connected. Changed
the cursor-visibility filter in both clients to `u.online` instead —
merge-eligibility and time-XP's own freshness checks are untouched
(server-side, `set_pos`/`time_xp_tick`), only what makes an already-online
player's cursor VISIBLE changed. Removed the now-fully-unused `is_present`
helper (both clients), `bin/web.rs`'s now-unused `UserRow.last_seen_micros`
field and its parser line (kept the wire's positional field indices correct
for everything after it), and `world::constants::PRESENCE_TIMEOUT_SECS`
(only ever read by the removed helpers — the server keeps its own separate
copy, unaffected).

**VERIFIED**: both clients build clean, no warnings (including after
removing the now-dead presence-check code — re-checked, this project treats
warnings as a build gate). Did not get a clean scripted multi-player capture
of the hover-open/hover-close timing in a real browser this batch (same
two-far-apart-islands framing difficulty as item 6's cursor-scale
verification) — logic was traced by hand instead (REASONED); the author's
own hand-test hovering a second real player's island is the real verification
here.

---

**Previous batch:** F9.5 item 6 — other players' cursors drawn screen-space
today, so zooming out leaves them huge relative to the tiles (and zooming in
leaves them small).

**Fix**: `world.rs` — `draw_cursor` now delegates to a new
`draw_cursor_scaled(d, m, color, scale)` (own-cursor call sites unaffected,
pass an implicit 1.0). Both clients compute `other_cursor_scale =
(camera.zoom / ISLAND_FIT_ZOOM).max(world::constants::CURSOR_MIN_SCALE)`
(new constant, 0.4) once per frame and pass it to every OTHER player's
cursor draw call — at the default connect zoom (13.0) this is exactly 1.0
(no change from before), scales down toward the 0.4 floor as the camera
zooms out past that, and scales up past 1.0 zoomed in (roughly tracking a
tile's own on-screen size either way, per the author's ask), never shrinking
past "still findable".

**VERIFIED**: both clients build clean. Own-cursor rendering re-screenshotted
at multiple zoom levels to confirm the `scale=1.0` default path is
byte-for-byte the same triangle geometry as before (no regression). Did
**not** get a clean visual capture of an actual OTHER player's cursor at two
different zoom levels — two independently-connected browser contexts land on
different, far-apart islands (`SLOT_SPACING`-scaled slot placement), and
getting one context's camera to simultaneously frame the other's island AND
have its cursor sit somewhere both zoom-level screenshots share proved too
fiddly to script reliably in the time available. The scaling math itself
(`(zoom / 13.0).max(0.4)`, applied through the identical, already-verified
triangle-draw path) is simple enough that this is REASONED rather than
VERIFIED for the actual other-cursor case — worth the author spot-checking
with two real windows during hand-testing, which trivially puts two cursors
in the same view.

---

**Previous batch:** F9.5 item 5 — modal click-through: clicking an overlay's
close button (Colors/Account/island-info) also painted or long-pressed the
map cell behind it, violating F3's "overlay is modal" rule.

Root cause (found the hard way — my first fix attempt didn't hold up under a
scripted repro, worth recording why): `map_input_allowed` gated on
`ui_state.overlay_open` etc AFTER `ui::handle_input` ran, so on the exact
frame a close click lands, the overlay is already closed by the time
`map_input_allowed` is computed — that frame's click then also reads as a
map click on whatever's behind it. My first attempt snapshotted the modal
state ONCE, before `handle_input`, and gated on both before/after — this
looked right and passed a naive same-frame test, but a REAL click's press and
release land on DIFFERENT engine frames (a mouse button held for even a
fraction of a second spans several frames at 60fps): the overlay closes on
the PRESS frame, but the button stays physically down for several MORE
frames before release, and every one of those saw "no modal open, mouse
still down" and let the same press paint anyway. Confirmed via a scripted
Playwright click (holding the simulated press for 150ms, realistic for an
actual click) against a local `spacetime start` instance, watching the
WebSocket for `CallReducer` frames: `paint_margin_cell` fired right after
closing the Colors overlay under the first fix.

**Fix**: replaced the one-shot snapshot with a per-gesture latch
(`suppress_map_until_release` — `main.rs`'s a local, `bin/web.rs`'s a new
`State` field since its frame function has no persistent locals otherwise).
Set once, at the moment `is_mouse_button_pressed(LEFT)` fires, to whatever
the modal-open state was AT THAT INSTANT; held unchanged for the rest of the
press regardless of how many frames later the overlay closes; cleared on
`is_mouse_button_released`. `map_input_allowed` now gates on this latch
instead of a same-frame snapshot.

**VERIFIED** live via the same Playwright/local-instance setup: after the
fix, holding a 150ms click on the Colors overlay's close button produces
ZERO `CallReducer` frames afterward (previously: a `paint_margin_cell` every
time); a fresh, unrelated click on the map immediately afterward still
paints normally (`paint_island_cell` fires), confirming the latch resets
correctly on release rather than sticking. Both clients build clean.

---

**Previous batch:** F9.5 item 4 — recent-colors (last-3 footer ring): two
author-caught bugs bundled together per plan.md ("same code area, do
together"):
1. **Not seeded on launch**: `UiState.last3` starts empty and was only ever
   populated by an explicit swatch click or a merge toast — a fresh
   connection with no merge yet left the footer's last-3 slots blank even
   though the player already has a starting color.
2. **Reset doesn't reset it**: `reset_account` wipes the whole inventory down
   to one fresh hue server-side, but the client's `last3` `VecDeque` was
   never told to forget its pre-reset entries — the existing merge-toast path
   (`note_used_hue`) would just prepend the fresh hue onto the two
   now-stale/invalid leftovers instead of replacing them.

**Fix**: added two `UiState` methods (`client/src/ui.rs`) — `seed_last3_once`
(no-op once `last3` has anything, so the caller can call it every frame after
`me` is known without its own seed-once flag) and `note_reset_hue` (clears
then reseeds with exactly one hue). Wired both into the existing
inventory-insert-watch block in `main.rs`/`bin/web.rs`: a newly-seen own
inventory row with `obtained_with: None` can now ONLY be `reset_account`'s
reseed (the very first such row, from `client_connected`, is already absorbed
by the pre-existing seeding pass before this loop ever runs; merges always
set `obtained_with: Some(partner)`), so that case routes to `note_reset_hue`
instead of the merge-toast path it used to fall into (which was itself a
latent, previously-unreported bug: a reset used to show a spurious "new
color, obtained with someone" toast).

**VERIFIED** live via the same Playwright-against-local-`spacetime start`
setup as prior items, both clients build clean:
- Screenshot on first connect: the footer's last-3 ring shows one swatch
  matching the header/border seed hue immediately, no merge needed.
- Screenshot after Account overlay -> Reset account (armed + confirmed) ->
  close: the island border re-rolled to a new hue and the footer shows
  exactly that ONE new swatch, not a mix of the old and new.

---

**Previous batch:** F9.5 item 3 — merge range too short. Single-constant
change: `server/src/lib.rs`'s `constants::MERGE_DIST` 1.0 -> 2.0
(cursor-merge trigger distance, checked entirely server-side in `set_pos` —
neither client needs a mirrored copy of this one). plan.md's constants table
already carried 2.0 as the settled value from the author's own note; applied
the matching server change to bring the code in sync with it.
**VERIFIED**: `cargo build -p server` clean; republished to the local
instance and re-ran `./generate_module_bindings.sh` — no schema change, so
bindings came out byte-identical (confirmed via `git status`/`git diff` on
`module_bindings/`, nothing to commit there). Did not re-verify the actual
merge-distance FEEL live (needs two concurrently-controlled cursors closing
to a specific distance, which is what plan.md explicitly reserves for the
author's own hand-test — "tunes by feel"); if 2.0 doesn't feel right on the
deployed build, adjust this constant and its plan.md table row together.

---

**Previous batch:** F9.5 item 2 — FPS at scale (author-reported 40fps @ 10
islands / 30fps @ 18 on the deployed build). Root cause matched plan.md's own
diagnosis exactly: both clients rebuilt a `HashMap<(island_id, q, r), color>`
from EVERY `island_cell` row in the world on EVERY frame
(`main.rs`/`bin/web.rs`, both just before `begin_drawing`), even though the
render loop right below already skips non-`in_view` islands — so the cost
scaled with total painted cells across the whole world, not what's on
screen.

**Fix**: added `world::island_cell_id(island_id, q_local, r_local) -> u32`
(mirrors `server::geometry::island_cell_id`'s packing exactly) and dropped
the per-frame HashMap entirely in both clients:
- `main.rs`: each rendered cell now does `ctx.db.island_cell().id().find(&id)`
  — an O(1) lookup through the SDK's own unique-index client cache (the same
  index `merge_target_at`/paint reducers already rely on server-side) instead
  of a full collect.
- `bin/web.rs`: `state.tables.island_cells` is already a
  `HashMap<u32, IslandCellRow>` keyed by the row's OWN id (its parser reads
  the `id` field as the map key) — which IS the packed cell id — so this one
  needed no lookup structure at all, just `state.tables.island_cells.get(&id)`
  directly against the existing per-row cache. The intermediate
  `(island_id, q, r) -> color` collect was pure redundant work.

Cost is now proportional to in-view cells (island count already filtered by
`in_view` above this loop, times the fixed 547-cell interior) instead of
every painted cell in the entire world, matching plan.md's asked-for fix
("build the map only from islands that are in_view").

**VERIFIED**: `cargo build -p client --bin client --bin bot` clean, no new
warnings. Rebuilt the web client and drove it with the same
Playwright-against-local-`spacetime start` setup as item 1's probe: painted a
multi-cell stroke via simulated mouse drag, screenshotted before/after
zoom-out — cells render with the correct color at both zoom levels, confirming
the id-packing/lookup swap didn't silently break rendering. Did **not**
re-measure actual FPS numbers under many-island load (would need several
real distinct identities painting concurrently, out of scope for a scripted
single-session probe) — the author's own hand-test with the deployed
bot/multi-player load is still the real verification plan.md asks for.

---

**Previous batch:** F9.5 item 1 — account import bug (CRITICAL, deployed-build
report: "pasting a token creates a NEW account instead of recovering the
existing one", reproduced by the author everywhere: standalone, itch embed,
cross-origin, local dev).

Root cause, found by scripted reproduction rather than inspection alone: spun
up a local `spacetime start` + published `hexmerge`, built `./build-web.sh`,
served `client/web` over `python3 -m http.server`, and drove two real
Chromium contexts with Playwright (installed into a throwaway venv for this
session) — one to mint an identity and read its token out of `localStorage`,
a second (fresh storage) to paste that token into the Account overlay's
import field and click Import, exactly as a player would. **VERIFIED** live:
before the fix, the token round-tripped through the import field truncated
and the post-reload identity matched neither browser's prior identity (a
THIRD, brand-new one each run) — confirming the server was rejecting the
imported token outright, not silently ignoring it. Isolated the truncation to
`ui.rs`'s import field: both the typed-input cap and the paste-room
calculation capped `import_input` at 256 chars, but a real SpacetimeDB
reconnect token observed on this instance is 386 chars — every paste silently
dropped the last ~130 chars into a corrupt JWT, which the server's `?token=`
auth rejects, so `game.html`'s `ws.onclose` handler (after `failedBeforeOpen
>= 2`) purged it and reconnected anonymously. This fully explains why it
reproduced in EVERY context (standalone/itch/cross-origin/local) — it's a
pure client-side field-length bug, unrelated to the wire protocol, origin
partitioning, or SpacetimeDB version. Also directly confirmed live (same
probe) that SpacetimeDB 2.6.1's `v1.json.spacetimedb` DOES honor `?token=` on
`/subscribe` and returns the SAME identity + token bytes for a valid token —
so plan.md's suspect (c) (the wire protocol itself) is ruled out; the other
listed suspects (a: does importToken reload — yes, confirmed; b: the
`failedBeforeOpen` purge — real, but a downstream symptom of the truncated
token failing to auth, not the root cause; d: TOKEN_KEY slot namespacing —
not implicated, both test contexts used the unslotted key and still failed
before the fix) are addressed by this same root cause.

**Fix**: `client/src/ui.rs` — replaced both hardcoded `256`s with a new
`IMPORT_TOKEN_MAX_LEN: usize = 2048` constant (generous headroom over the
~386-char tokens actually observed, not a tightly-fitted bound — SpacetimeDB
could grow the claim set). Re-ran the exact same Playwright probe after
rebuilding: the imported token now round-trips byte-for-byte and the
post-reload identity matches the source browser's original identity exactly.
`cargo build -p client --bin client --bin bot` and `cargo build -p server`
both still clean.

Not yet done: the author's own hand-test on the actual DEPLOYED build (itch
embed + cross-origin, per plan.md's verify checklist) — this batch's
verification used a local `spacetime start` instance and a scripted browser,
which nails the root cause and confirms the fix mechanically, but doesn't
substitute for the real hand-test plan.md asks for before this item is
checked off for the freeze.

---

**Previous batch:** F9 (island links + XP economy) implemented in one batch —
schema-additive server change (two reducers, one new table, one new
repeating scheduled reducer, no data wipe of existing tables) plus client
work (native and web share `ui.rs`, kept identical as always).

Server (`server/src/lib.rs`):
1. **`set_island_link(rate_id: u32)`** — caller's own island's
   `itch_rate_id` is set/replaced (decision 14: just the numeric itch
   submission id). **VERIFIED** live via `spacetime call hexmerge -s local
   set_island_link 12345` against the CLI's own auto-provisioned identity,
   then `spacetime sql`: the owning island's `itch_rate_id` shows `(some =
   12345)`.
2. **`click_link(island_id: u32)`** — new `island_link_click` table (exact
   `IslandLike` dedupe pattern: one row per (island, clicker), checked in
   the reducer rather than a DB-level compound unique) credits the owner
   `XP_LINK_CLICK` once per clicker. **VERIFIED** three paths live via
   `spacetime call`: unknown island → `"unknown island"`; an island with no
   link set → `"island has no link set"`; the caller's own (linked) island →
   `"cannot credit your own link click"` (self-click guard, mirrors
   `like_island`'s). The success + dedupe path (second click from the SAME
   clicker grants no further XP) is **REASONED** only — it's a straight
   copy of `like_island`'s already-live dedupe logic, and exercising the
   success path from the CLI would need a second distinct identity, which
   isn't practical to script here; two-client hand-testing should upgrade
   this to VERIFIED.
3. **Time XP**: new `time_xp_schedule` (repeating, `TIME_XP_PERIOD_SECS` =
   60s, lazy-seeded in `client_connected` exactly like `rerank_warn_schedule`)
   drives `time_xp_tick`, which grants `XP_TIME` (1) to every `online` user
   whose `last_seen` is within the same presence window used everywhere else
   (cursor visibility, merge eligibility) — plan.md's own wording ("users
   with fresh last_seen") ties it to that, not just "connected". **REASONED**:
   the schedule row inserts and the reducer's self-only-caller guard mirrors
   `rerank_warn`/`rerank_fire` exactly; the actual 60s tick firing wasn't
   sat through live in this batch (would need a longer-lived local session
   than this batch's CLI probing). Confirm with `spacetime sql hexmerge -s
   local "SELECT xp FROM user"` climbing by 1/minute for an idle-but-online
   connected client during hand-testing.

Client (`client/src/ui.rs`, shared by native + web):
4. **Island-info popup**: own-island view now shows the current link plus a
   digits-only edit field + "Set" button (seeded from the current
   `itch_rate_id` each time the popup opens, Enter or the button submits);
   foreign-island view renders a set link in a distinct color with a
   "(click to open)" hint, wired to a new `link_row_rect()` hit test that
   only fires when a link is actually set. **REASONED** (code-reviewed,
   builds clean on both `cargo build -p client --bin client` and
   `./build-web.sh`) — not run through the actual GUI this batch, per the
   standing rule that the author drives native hand-testing themselves.
5. **Link click → open URL**: native fires `raylib::open_url` (from
   `raylib::prelude`) with the itch rate URL directly; web runs
   `window.open(url, '_blank')` via the existing `run_js`/JSON-escaping
   pattern (mirrors `call_reducer`'s escaping, even though `rate_id` is
   server-typed numeric so injection isn't really reachable here). Both
   fire `click_link` alongside the URL open. **REASONED**, same caveat as
   above — an itch-iframe popup-blocker risk on the web path specifically is
   worth watching for in hand-testing (see plan.md's F6 precedent on iframe
   restrictions), no fallback was built preemptively since plan.md's F9 spec
   states `window.open` directly.
6. **Level-up toast**: both `main.rs` and `bin/web.rs` now track
   `last_level` (seeded on the first frame `me` is known, exactly like the
   existing `inventory_seeded` pattern) and call the new
   `UiState::show_levelup_toast` when `level_of(xp)` increases, reusing the
   existing merge-toast rendering. The Saturation slider's max already grows
   live every frame from `HudInfo::sat_cap` with no code change needed — the
   toast is the only piece plan.md's "level-up feedback" item required.
   **REASONED**, not hand-tested (would need enough merge/like/link-click XP
   in one sitting to cross a level boundary).

Builds: `cargo build -p server`, `cargo build -p client --bin client`,
`cargo build -p client --bin bot`, and `./build-web.sh` all clean
(`du -sh client/web` = 932K, well under the 64 MB jam cap). Bindings
regenerated (`set_island_link_reducer.rs`, `click_link_reducer.rs`,
`island_link_click_table.rs`, `time_xp_schedule_type.rs` added).
plan.md's canonical constants table gained `XP_TIME`/`TIME_XP_PERIOD_SECS`
(F9 already specified the 1 XP / 60 s values inline; this just adds them to
the single-source-of-truth table alongside the existing `XP_LINK_CLICK` row).

Not yet done, left for the author's hand-test session before checking F9 off
below: the two-client success/dedupe path for `click_link`, the 60s time-XP
tick actually firing, the popup's new edit field/link row end-to-end (typing,
Set, click-to-open on both native and a browser), and the level-up toast.

---

**Previous batch:** a third round of author feedback on F8 — a schema-additive
server change (new reducer, no data wipe) plus client work (native and web
kept identical as always):
1. **"We should see Owner, Likes, Created and Link"** — checked: the popup
   already renders all four (`draw_island_popup`, `ui.rs`), unchanged this
   batch. `Link` currently always reads "not set" since `set_island_link` is
   F9, not yet built — expected at this stage, not a bug.
2. **"The color of the border should be the same saturation and value as the
   default (40 and 100)"** — the previous batch's border-hue fix used a
   fixed `(85, 95)` lookalike shade instead of the owner's ACTUAL starting
   color. Changed to `(START_SAT, 100)` = `(40, 100)` exactly, in both
   clients.
3. **"Sometimes a border vanishes (just one side) when we dezoom"** — root
   cause: border thickness was a fixed WORLD-unit value (`0.4`/`0.15`), and
   `BeginMode2D`'s zoom scales line geometry along with everything else — at
   low zoom the line fell under a screen pixel, and the hexagon's six edges
   (each at a different angle) round to zero at slightly different zoom
   levels, so one side would disappear before the others. Fixed by
   converting a constant SCREEN-pixel width (3px mine / 1.5px foreign) back
   to world units by dividing by `camera.zoom` each frame, so the rendered
   line stays a constant, always-visible width regardless of zoom.
4. **"We should be able to unlike an island"** — added `unlike_island`
   reducer (server): removes the caller's `island_like` row, decrements
   `island.likes`, and reverts the owner's `XP_LIKE` grant (`saturating_sub`)
   — reverting the XP is necessary, not optional: without it a
   like/unlike/like cycle would let one liker re-grant the owner XP
   indefinitely, since `like_island`'s uniqueness check only looks at
   CURRENTLY-existing rows. Bindings regenerated
   (`unlike_island_reducer.rs`), republished non-destructively (additive
   reducer only, no column change, no data wipe). The popup's Like button
   now toggles both ways ("Like" <-> "Unlike") instead of going inert once
   liked.
5. **"Double-clicking should like, with a little animation"** — asked the
   author where this should live, since a single click already opens the
   (modal) info popup which then blocks further map input, so a naive
   double-click-on-the-map handler would never see its second click. Author
   chose Instagram-style: double-click the island directly on the map, no
   popup involved. Implemented as a deferred-open two-stage gesture: a clean
   single click on a foreign island is held as `pending_info_click` for
   `DOUBLE_CLICK_WINDOW` (350ms) instead of opening the popup immediately;
   if a second click lands on the SAME island within that window (and inside
   `LONG_PRESS_TOL_PX`), it's consumed as a like/unlike toggle (calling
   `like_island`/`unlike_island` directly, whichever applies) instead, and
   the popup never opens for that click pair. If no second click arrives,
   a separate per-frame check opens the popup once the window elapses. The
   trade-off, understood and accepted: every single click now opens the
   popup ~350ms later than before, instead of instantly.
   - New `UiState.like_anims` (shared `ui.rs`, both clients get it for
     free): a floating "+1"/"-1" with a growing, fading colored ring at the
     click position, `LIKE_ANIM_DURATION` = 600ms, pruned in `handle_input`
     alongside the toast, drawn in `draw`.
- `cargo build -p server` clean; `spacetime generate`/`publish -s local
  --delete-data=on-conflict` both succeeded with NO data wipe (additive
  reducer only). `cargo build -p client --bin client --bin bot`,
  `./build-web.sh` (release), `cargo build --workspace --exclude client` all
  clean, no warnings (forced recompile via `touch`). `du -sh client/web` =
  924K.
- VERIFIED: author hand-tested and confirmed it feels good. Committed
  together with all earlier F8 batches below.

---

**Previous batch:** two more author-caught fixes on top of F8, from a second
round of live hand-testing (no schema/server change — client-only, both
`main.rs` and `bin/web.rs` kept identical as always):
1. **"Clicking on someone else's island (release mouse) should show that
   popup, not just the center"** — `info_target` was additionally filtered to
   `hexdist(lq, lr) <= INFO_HIT_RADIUS` (3 hex units from the island's exact
   center), left over from before the previous batch removed the `owner !=
   me` restriction. Dropped entirely: `island_at` already bounds the hit test
   to the island's full `ISLAND_RADIUS` footprint, so any clean click
   anywhere on a FOREIGN island now opens its popup. `INFO_HIT_RADIUS` is now
   dead (only that one call site used it) and was deleted from both clients.
2. **"You should have a button in the footer rather than clicking on your own
   island (in which you draw)"** — the previous batch's "click your own
   island's center also opens the popup" fix DID open the popup, but paint
   is a separate, independent gesture that fires on press regardless (that's
   the existing paint-while-held behavior) — so every click meant to check
   your own likes also painted over that cell with your current brush,
   an unwanted side effect. Replaced with a dedicated footer button:
   `ui::Actions.open_own_island`, a new `my_island_btn_rect()` ("My Isle",
   footer-right, next to Account), wired in both `main.rs`/`bin/web.rs` to
   call `open_island_info` on `my_island(...)`'s result. Own-island clicks
   now only paint, with no popup side effect; the popup is reached
   exclusively via the button, which never touches the canvas.
- `cargo build -p client --bin client --bin bot`, `./build-web.sh` (release),
  `cargo build --workspace --exclude client` all clean, no warnings (forced
  recompile via `touch`). `du -sh client/web` = 924K. No server/schema
  change this batch.
- VERIFIED: author hand-tested and confirmed it feels good. Committed
  together with all earlier F8 batches below.

---

**Previous batch:** four author-caught fixes on top of F8, from live
hand-testing (no schema/server change — client-only, both `main.rs` and
`bin/web.rs` kept identical as always):
1. **"How to see your own likes?"** — the info-popup click gesture
   (`info_target`) was filtered to `owner != me`, so there was literally no
   way to open your own island's popup. Filter dropped; a click on your own
   island's center now also opens the popup (it still paints that one cell
   too, unchanged — the two aren't mutually exclusive). The Like button
   stays hidden for your own island regardless (`IslandInfo.is_own`), so this
   only adds a read-only view of your own stats, no new self-like path.
2. **"Fill the border of an ilot with the first color you get when creating
   the account"** (design request, not a bug) — every island's border used
   to be undrawn except a fixed gold outline on your OWN island only. Now
   EVERY island's border is drawn in its owner's SEED hue (`seed_hues` /
   `seed_hues` maps built once per frame from the inventory rows where
   `obtained_with(_hex).is_none()` — exactly one such row exists per owner
   at any time, either the original `client_connected` seed or the latest
   `reset_account` reseed), at a fixed vivid `(sat 85, val 95)` regardless of
   the owner's live/nudged brush, so it reads as a stable identity marker
   rather than flickering with their slider. Own island keeps a thicker line
   (0.4 vs 0.15 world units) so "which one is mine" is still a glance away,
   just via thickness now instead of a different color.
3. **"Like button isn't reactive (must reload to see I liked it)"** — the
   popup's `likes`/`already_liked` were a ONE-TIME snapshot taken by
   `open_island_info` at click time; nothing ever refreshed them afterward,
   so a successful `like_island` call had no visible effect until the next
   popup open (or reload). Added `UiState::refresh_island_popup`, called
   every frame the popup is open (re-reading the live `island`/`island_like`
   rows), mirroring how `HudInfo` itself is already rebuilt fresh every frame
   rather than cached.
4. **"Like button is on top of the link, inconvenient"** — `like_btn_rect()`
   sat at `overlay_y + 140`, overlapping the link row drawn at
   `overlay_y + 132` (16px font). Moved to `overlay_y + 170`, clear of it;
   the "(this is your island)" label shifted to match.
- `cargo build -p client --bin client --bin bot`, `./build-web.sh` (release),
  `cargo build --workspace --exclude client` all clean, no warnings (forced
  recompile via `touch`). `du -sh client/web` = 924K. No server/schema
  change this batch, so no republish/bindings-regen needed.
- VERIFIED: author hand-tested and confirmed it feels good. Committed
  together with all earlier F8 batches below.

---

**Previous batch:** F8 (likes, island info, 5-minute re-ranking) implemented.

**Schema (breaking):** `Config` gained `next_rerank_at: Option<Timestamp>`.
New tables: `IslandLike` (`public`; `#[derive(Clone)]` added to `Island` too,
needed by the reslot logic below), and two SERVER-INTERNAL (not `public`,
correctly excluded from codegen — confirmed by `generate_module_bindings.sh`'s
own "Skipping private tables" log line) scheduled tables:
`RerankWarnSchedule` (repeating, `RERANK_PERIOD_SECS`=300) and
`RerankFireSchedule` (one-shot, scheduled `RERANK_WARNING_SECS`=5 later by
the warn step). Both scheduled reducers (`rerank_warn`, `rerank_fire`) guard
`ctx.sender() != ctx.database_identity()` per the SDK's documented pattern —
confirmed live: a raw `spacetime call` can't reach them (they're not even in
the generated client-callable reducer list, since their sole argument type
is a private table row).
- **IMPORTANT — data wipe (author should know):** adding `next_rerank_at` to
  the existing `Config` table is NOT a compatible schema change on this SDK
  version ("Adding a column ... requires a default value annotation") — the
  local dev database got a full `--delete-data` wipe on the FIRST publish of
  this batch (all prior local test users/islands/painted tiles gone). This
  was NOT anticipated going in; flagging clearly since it's a real, if
  local-only, data loss. Every subsequent publish this batch (constants-only
  changes, tested live below) was a compatible/empty migration plan — no
  further wipes.
- `rerank_warn`/`rerank_fire` are lazily seeded in `client_connected`
  (`if ...count() == 0 { insert(...) }`), the same pattern already used for
  the `config` row — NOT an `init` reducer, because `init` does not re-run
  on a republish of an existing (non-cleared) database, which would have
  silently left the re-rank timer never started on any redeploy after the
  first.
- Slot reassignment in `rerank_fire`: `slot` is `#[unique]`, so a direct
  permutation risks one island's NEW slot colliding with another
  still-unmoved island's CURRENT slot (both real values, same unique index).
  Reslots in two passes — everyone to a temporary `slot + 1_000_000` range
  first, then to the final `1..N` — sidestepping the collision entirely.
- `like_island`: self-like rejected (`island.owner == ctx.sender()`);
  (island, liker) uniqueness enforced in the reducer (one row check), not as
  a DB constraint — this SDK only supports single-column `#[unique]`, no
  compound constraints.
- Client (both, kept identical): a short click (released before the
  400 ms merge-hold threshold, no drift past `LONG_PRESS_TOL_PX`) on a
  FOREIGN island's center — within `INFO_HIT_RADIUS`=3 hex of its slot
  center — opens an "Island" info popup (owner name, likes, relative age,
  link-if-set, Like button). Reuses the existing `LongPress` gesture
  bookkeeping (`info_target` alongside the existing merge `target`) rather
  than a new gesture type, since the two are already naturally distinguished
  by hold duration; own-island clicks never trigger it (they paint, as
  before) since `info_target` is filtered to `owner != me` at press time.
  `ui.rs` gained a THIRD mutually-exclusive modal (`island_popup`, alongside
  `overlay_open`/`account_open`) plus a non-modal rerank-countdown banner
  (`HudInfo.rerank_secs`, computed by each caller from `config.next_rerank_at`
  vs the local clock — `Timestamp::duration_since` natively on the client
  side, raw micros arithmetic on the web side, same shape as every other
  native/web time comparison in this codebase).
- Age formatting: no date/time crate in the workspace: `format_age` buckets
  into "just now"/"Nm ago"/"Nh ago"/"Nd ago" from integer seconds — plenty
  for a jam popup, no new dependency.
- Web-only note: `bin/web.rs`'s `config`/`island_like` tables were added to
  its generic `RowView`-based parser (same pattern as every other table);
  `IslandRow` gained `likes`/`itch_rate_id`/`created_at_micros` (previously
  only `owner_hex`/`slot` — nothing needed them before F8). `game.html`'s
  `TABLES` subscription list updated to include `island_like`.

**VERIFIED live** (local `spacetime` 2.6.1, via `spacetime call`/`sql`/`logs`,
not yet through an actual browser/native UI — that part is REASONED from
code, BLOCKED on author hand-test):
- `like_island`: a fresh `--anonymous` identity liking island 1 → likes
  0→1, `island_like` row inserted, owner's xp +10 (`XP_LIKE`). Calling it
  again as the OWNER identity → rejected `"cannot like your own island"`.
  Calling with a bogus id (999) → rejected `"unknown island"`.
- Full two-step timer chain, observed with `RERANK_PERIOD_SECS`/
  `RERANK_WARNING_SECS` temporarily dropped to 10/3 (reverted after,
  republished, confirmed back to 300/5 via `spacetime sql`): the repeating
  loop fired `rerank_warn` (`config.next_rerank_at` went `None` → `Some`),
  `RerankFireSchedule` gained a one-shot row, then ~3s later `rerank_fire`
  ran and `next_rerank_at` went back to `None` — no `ERROR` lines in
  `spacetime logs` either time.
- **The actual slot permutation**, not just the chain executing as a no-op:
  manufactured a real ranking change (liked island 3 three times, more than
  island 1's two, via three distinct `--anonymous` identities), waited for
  the next cycle, and confirmed island 3 → slot 1, island 1 → slot 2 — the
  two-pass temp-offset reslot handled a genuine unique-constraint collision
  correctly, not a coincidentally-already-sorted no-op.
- `cargo build -p server`, `-p client --bin client --bin bot`,
  `./build-web.sh` (release), `cargo build --workspace --exclude client` all
  clean, no warnings (forced recompiles via `touch` to rule out stale
  incremental-build caching). `du -sh client/web` = 916K, well under the
  64 MB cap.
- Not yet exercised: the actual popup/Like-button UI in a running client (no
  browser/second native instance available to me this batch — same
  constraint noted in every prior batch); the rerank countdown banner
  rendering; touch/mobile info-click on web.

**Previous batch:** F6 (identity: token login, reset) implemented, plus three
author-caught fixes found via live hand-testing during the same batch
(`known_bugs.md`, a scratch note the author was actively filling in while I
worked — checked in as untracked, left alone). Client: added an "Account"
overlay (`ui.rs`), a new footer button opening it, mutually exclusive with
the inventory overlay (opening one closes the other; `map_input_allowed` in
both `main.rs` and `bin/web.rs` now also gates on `account_open`). Contents:
Copy-ID button, an import-token field (web only, see below), and a
Reset-account button that arms on first click ("Click again to confirm
reset") and fires on a second click within `RESET_CONFIRM_WINDOW` (4s),
auto-disarming otherwise. `HudInfo` gained `show_token_import: bool` (true on
web, false on native) so `ui.rs` stays platform-clean while only rendering
the paste-token field where it's actually wired up.
- **Copy ID**: "ID" here means the full reconnection TOKEN (plan.md decision
  11 — the token, not the identity, is the Cookie-Clicker-style recovery
  key), not the header's 8-hex-char display label. Native: `Actions::copy_token`
  triggers a fresh `creds_store().load()` (re-reading rather than holding a
  stale copy from startup, in case a reconnect rotated it) +
  `rl.set_clipboard_text`. Web: triggers `window.stdb.copyToken()`
  (`game.html`), which tries `navigator.clipboard.writeText` and falls back
  to `window.prompt` (pre-selected text, so Ctrl+C works even where the
  Clipboard API is denied — e.g. a sandboxed itch.io iframe) — satisfies
  plan.md's "always provide a fallback that shows the token in a selectable
  text field" without any extra DOM/CSS.
- **Import (web only)**: text field + Import button/Enter in the Account
  overlay, wired to `window.stdb.importToken(token)`, which writes
  `localStorage` and reloads the page (auth is a connect-time query param in
  this protocol, not a live call, so there's no in-place identity hot-swap —
  a reload was explicitly called out as acceptable in plan.md). Native leaves
  `show_token_import: false`, so the field never renders and
  `actions.import_token` is structurally always `None` there — matches
  plan.md's explicit allowance to SKIP native import as out of scope for the
  jam (time-boxed).
- **Reset**: `actions.reset_account` → `ctx.reducers.reset_account()` /
  `call_reducer("reset_account", [])`.
- **BUG FIX (author-caught, `known_bugs.md`): "reset an account should give a
  random color"** — `reset_account` (`server/src/lib.rs`) was rolling the new
  hue via `start_hue(&ctx.sender())`, the SAME deterministic hash of the
  identity used at first connect. `client_connected` keeping that
  deterministic function is fine (a brand-new identity is itself effectively
  random), but `reset_account` keeps the SAME identity by design (decision
  12) — so every reset was silently handing the player back their exact
  original starting hue, never a fresh one, defeating the whole point of the
  reducer's name. Fixed to draw from `ctx.rng().gen_range(0..360u16)`
  instead (`spacetimedb::rand::Rng` imported). `start_hue` is now only used
  where determinism is actually correct: `client_connected`.
- **BUG FIX (author-caught): "you can't paste in 'Paste an ID'"** — the
  import field's input handling (`ui.rs`, added this batch) only captured
  typed characters (`get_char_pressed`) and Backspace, like the (much
  shorter) name field it was modeled on. A ~200+-char token isn't realistic
  to type by hand. Added Ctrl+V/Cmd+V handling that reads
  `rl.get_clipboard_text()` and appends the trimmed result, capped at the
  same 256-char field limit.
- **Design addition (author-directed, in response to a question about the
  two remaining `known_bugs.md` items):** "merged with ... shouldn't include
  the id of someone" / "id is very confidential" — the merge toast's
  `player_label` fell back to the partner's short identity hex when they had
  no name set, which leaks enough of the identifier to correlate a player
  across merges (the same identifier decision 11 requires to stay
  confidential, since it doubles as the account-recovery token). Author's
  fix of choice: every player gets a random generated name at first connect
  instead of `name: None`, so the fallback essentially never fires. Added
  `random_name` (`server/src/lib.rs`): a 20-adjective × 20-noun table (e.g.
  "SwiftFox"), sampled via `ctx.rng()`, set in `client_connected`'s
  new-user branch. Hardened the fallback itself too, in both clients'
  `player_label` (`main.rs`, `bin/web.rs`): now `"another player"` instead of
  `short_hex(id)`, as defense-in-depth for any pre-existing row that
  predates this change (local dev data only). The OTHER `known_bugs.md` item
  ("selected color not in recent-used on launch") was explicitly deferred by
  the author — not part of this batch, not fixed.
- Also added a confidentiality warning line next to the Copy-ID button
  ("Keep it private: anyone who has it can log in as you"), directly from
  the "id is very confidential" note.
- `cargo build -p server`, `-p client --bin client --bin bot`, `./build-web.sh`
  (release) all clean, no warnings (re-checked with `touch` to force a
  recompile after the no-warning first pass, since incremental builds can
  hide a fresh warning). `cargo build --workspace --exclude client` clean.
  `du -sh client/web` = 892K (well under the 64 MB cap). Author independently
  confirmed the build succeeds after the RNG/paste/naming fixes.
- REASONED from code, not yet run live for actual behavior (needs a browser +
  a real `spacetime start` instance): copy/paste round-trip, reset actually
  producing a fresh hue in practice, refresh/reopen identity persistence
  (already implemented at F5 via `localStorage`, not newly touched here — F6
  just adds the recovery UI plan.md asked for on top of it), random names
  appearing for new connections, merge toast never showing an identity hex.
**Blockers:** none.

---

**Previous batch:** Author-caught fix on top of F5: long-press-to-merge on a
margin tile didn't work — the tile's color got instantly overwritten by the
ordinary paint-on-press before the 400ms hold timer could fire
`merge_with_cell`, so by the time the merge landed the tile already matched
the caller's own brush and the server correctly (but unhelpfully) rejected
it as a no-op. **Design ruling (author):** rather than deferring the paint
until release (workable but adds real complexity for a zone that's supposed
to be low-stakes), margin tiles are no longer a color-discovery source at
all — long-press-to-merge now only ever targets ISLAND cells.
`merge_target_at` (native `main.rs` and `bin/web.rs`, kept identical) dropped
its margin-cell branch entirely; `merge_with_cell` (`server/src/lib.rs`)
dropped its `cell_kind == 1` arm, so a raw call bypassing the client is
rejected the same way as any other invalid `cell_kind`. Margin tiles remain
normally paintable by everyone, unaffected. `cargo build -p server`,
`-p client --bin client --bin bot`, `cargo check --target
wasm32-unknown-emscripten`, and `cargo build --workspace --exclude client`
all clean. Republished non-destructively (`--delete-data=on-conflict`, no
schema change). Committed (`fb0b120`).
**Blockers:** none.

---

**Previous batch:** F5 (web client parity) implemented in one batch: `ui.rs`
made genuinely platform-clean (dropped its `spacetimedb_sdk::Identity`
dependency — `HudInfo.me` replaced with a precomputed `short_id: &str`,
supplied by `main.rs`/`web.rs` respectively), then `client/src/bin/web.rs`
rewritten from scratch to mirror the full F1 schema (`config`, `user`,
`inventory`, `island`, `island_cell`, `margin_cell`) and every P0 reducer,
reusing `world.rs`/`ui.rs` verbatim (`#[path]`-included) instead of
duplicating geometry/HUD code as the old pre-F1 web client did. `game.html`
updated: subscribes to all 6 tables, token storage moved
sessionStorage → localStorage, with a `?slot=` query param namespacing the
storage key so `index.html`'s dual-iframe two-player harness keeps getting
distinct identities (localStorage, unlike sessionStorage, is shared across
same-origin iframes unconditionally, so this was a real requirement, not
just a nice-to-have). Wire-format assumptions (`Option<T>` encoding,
`Identity`/`Timestamp` shapes in both named and positional row encodings)
were not guessed: verified live with a throwaway read-only WebSocket probe
against the already-running local `spacetime` 2.6.1 instance (subscribed,
inspected `InitialSubscription`/`TransactionUpdate`, called `set_name`/
`set_pos`/`paint_island_cell`/`paint_margin_cell` from a scratch identity)
before writing the parser. `cargo check -p client --bin web --target
wasm32-unknown-emscripten`, `cargo build -p client --bin client --bin bot`,
`cargo build --workspace --exclude client`, and `./build-web.sh` (release)
all clean, no warnings. `du -sh client/web` = 880K (well under the 64 MB
jam cap). Not yet hand-tested live (no browser available to me) or
committed — author to hand-test per the F5 verify checklist (dual-iframe
paint+merge, phone pinch/pan/paint/long-press) before commit.
**Blockers:** none.

---

## F1 — Server: schema + reducers
- [x] Data model, geometry helpers, merge formula in `server/src/lib.rs` — VERIFIED
      (`cargo build -p server` clean, no warnings).
- [x] `./server/publish.sh` succeeds — VERIFIED against local `spacetime start`
      (pre-existing instance, pid discovered via lock error; not started fresh).
- [x] All tables visible via `spacetime sql hexmerge` — VERIFIED: `config`, `user`,
      `inventory`, `island`, `island_cell`, `margin_cell` all present and queryable.
- [x] `spacetime call hexmerge set_name '"x"'` hits the unknown-user error path —
      **DEVIATION**: does NOT hit that path. The installed CLI is 2.7.0
      (`~/.local/bin/spacetime`) — plan.md's pinned `~/.cargo/bin/spacetime` v2.6.1
      does not exist on this machine, and `client/Cargo.toml` already documents the
      project moved to 2.7.0 for the SDK too. Under 2.7.0, `spacetime call` (even
      `--anonymous`) synthesizes a full connect→call→disconnect cycle, so
      `client_connected` fires before the target reducer runs — confirmed by
      watching `spacetime logs` (rapid connect/disconnect pairs) and by `set_name`
      succeeding for a never-before-seen `--anonymous` identity (new `user` +
      `island` row created, name set). VERIFIED (behavior), REASONED (root cause).
      The `ok_or("unknown user")` guards are REASONED correct from code — they are
      real dead-code-safety nets, just not reachable via this CLI's one-off calls.
- [x] Behavioral rules (ownership, rate limit, merge) — deferred to F2, listed as
      REASONED (traced in code, not yet exercised): ownership check in
      `paint_island_cell`, rate limiting in `take_paint_token`, merge formula in
      `merge::merge_hue`, cursor-merge scan in `set_pos`.

## F2 — Native client: world, camera, paint
- [x] `world.rs` (geometry/constants mirror), bindings regenerated, `main.rs`
      rewritten — VERIFIED: `cargo build -p client --bin client` clean, no
      warnings. `hexgrid.rs` deleted (folded into `world.rs`).
- [x] Camera: wheel zoom (0.25–4.0), SHIFT/middle drag pan, starts on own
      island — REASONED from code (official raylib zoom-toward-cursor
      recipe; pan via `get_mouse_delta`/zoom; camera re-centers on
      `my_island()` once the Island row appears). NOT yet visually run.
- [ ] Two instances: mutual islands + live cursors — BLOCKED, local
      `spacetime start` is down (author reinstalling CLI 2.6.1).
- [ ] Paint own island OK; painting the other player's island rejected —
      BLOCKED, same reason. Client-side `classify()` in `main.rs` only ever
      sends `paint_island_cell` for the caller's own island interior or
      `paint_margin_cell` for margin; never sends a click on someone else's
      island anywhere (REASONED from code), so the server-side rejection
      path is defense in depth here, not the primary guard.
- [ ] Rate limit felt in-game and confirmed via `spacetime sql` counts —
      BLOCKED, same reason.
- [ ] F1 REASONED items exercised here upgraded to VERIFIED — BLOCKED, same
      reason.

Smoke test done: launched `cargo run -p client --bin client` against a
running local instance before it was taken down for the CLI switch — it
opened, connected, and only failed once the instance was stopped
(`ConnectionRefused`), confirming the binary itself starts cleanly.

**Re-verified against spacetime 2.6.1** (author reinstalled it): republished
(`./server/publish.sh`), regenerated bindings, whole-workspace build check:
- `cargo build -p server` — VERIFIED clean.
- `cargo build -p client --bin client` — VERIFIED clean, no warnings.
- `cargo build -p client --bin bot` — VERIFIED clean. Turns out `bot.rs` only
  calls `set_name`/`set_pos`, both reducer-compatible with the new schema —
  it was NOT actually broken by F1 like plan.md anticipated. Correcting the
  earlier assumption: only `web.rs` needs the bot-style exception.
- `./build-web.sh` — confirmed still fails the same way (`mod hexgrid` +
  underlying schema mismatch), per the author-approved decision above.

## F3 — Color picker, inventory, HUD (native)
- [x] `ui.rs` created: 28px header (short identity hex + level/xp, online/total
      count), 44px footer (Center button, last-3-hue swatches, inventory
      toggle, name text field, Lock toggle), modal inventory overlay
      (unlocked-hue grid at `sat_cap`, saturation slider capped at
      `SAT_CAP(level)`, value/luminosity slider 0–100 uncapped) — VERIFIED by
      the author hand-testing across several review rounds (hover highlight,
      cursor z-order, live swatch color, and Center button zoom/offset bugs
      were all caught this way and fixed). Fits 720×720, overlay is modal.
- [x] `set_brush`/`set_name`/`set_lock` wired from `ui::Actions` in
      `main.rs` — VERIFIED via hand-testing (hue/saturation/value picking
      and Center all confirmed working end to end). Sat slider's `max`
      argument is `info.sat_cap` (`world::sat_cap(level)`), so it's
      structurally impossible to drag past the cap; last-3/swatch clicks
      re-clamp `sat.min(sat_cap)` defensively for the same reason.
- [ ] Name persists across restart (sql check) — BLOCKED on author hand-test
      (needs a live `spacetime start` + two-instance run, same as F2).

Implementation notes:
- Text field editing captures `rl.get_char_pressed()`/backspace directly in
  `ui::handle_input`, called once per frame before `begin_drawing` (mirrors
  the existing `other_cursors` pattern in `main.rs` — draw-handle borrows
  can't coexist with `&mut RaylibHandle` input calls). Name commits to
  `set_name` on Enter or on click-away (blur); the field is seeded from the
  server's `user.name` exactly once (`sync_name_once`) so the subscription
  echoing our own edit back doesn't clobber in-progress typing.
- No `measure_text` available on the draw handle (that method is inherent to
  `RaylibHandle`, not part of the `RaylibDraw` trait, so it's unreachable
  once `begin_drawing` hands out its borrow) — the header's right-side
  online/total label uses a fixed x position instead of right-aligning to
  measured width. Cosmetic only, flagged in case the author wants a tighter
  layout later.
- Removed the F2 placeholder TAB-cycle/+/- brush controls per plan.md
  ("replaced in F3"); the help text is gone too since the footer/header now
  show that information directly.
- **Fix (author-caught):** hover highlight in `main.rs` used to draw over
  ANY cell under the mouse, including other players' islands and while the
  inventory overlay was open — misleadingly implying a paint the server
  would reject. Now gated on `classify(...) != Paintable::None` and
  `map_input_allowed` — VERIFIED via `cargo build -p client --bin client`
  clean; visual behavior not yet re-run live.
- **Fix (author-caught):** own cursor was drawn BEFORE `ui::draw`, so the
  header/footer/overlay painted over it — impossible to see your own
  pointer while hovering the HUD. Reordered so `ui::draw` runs first and the
  own-cursor triangle is drawn last (other players' cursors stay under the
  HUD, only the caller's own needs top z-order) — cargo build clean, not yet
  re-run live.
- **Fix (author-caught):** swatches (footer last-3, inventory grid) were
  rendered at fixed `(sat_cap, 90)` regardless of the live slider values, so
  dragging saturation/value only visibly changed the cursor, not the HUD.
  Added `swatch_color()`: the swatch matching the CURRENT brush hue now
  renders at the actual `info.brush.1/.2`, live with the sliders; other
  (non-selected) hues stay static previews at the sat cap since they aren't
  the active brush — cargo build clean, not yet re-run live.
- **Fix (author-caught):** centering (startup + footer button) only moved
  `camera.target`, leaving zoom untouched — so "center" didn't actually put
  the island in view at a useful scale, and the startup zoom (2.0, a
  leftover placeholder) was far too zoomed out regardless. Added
  `ISLAND_FIT_ZOOM = 13.0` (sized from `ISLAND_RADIUS`'s ~22.5-world-unit
  reach against the 720×720 window minus header/footer bands) and set
  `camera.zoom` to it at both the initial auto-center and the Center-button
  press. Also renamed the footer button label `"[+]"` → `"Center"` (widened
  its rect from 30px to 54px and shifted every rect to its right by 24px so
  nothing overlaps) — cargo build clean, exact fit not yet eyeballed live.
- **Fix (author-caught):** Center button zoom was right but the island
  landed off-screen-center. Root cause: mouse-wheel zoom re-anchors
  `camera.offset` to the cursor position (the zoom-toward-cursor recipe), so
  after any prior scroll `offset` was left wherever the mouse last
  scrolled — centering only reset `target`/`zoom`, not `offset`. Both
  centering spots (startup auto-center, Center button) now also reset
  `camera.offset` back to screen-center `(360, 360)` — cargo build clean,
  not yet re-run live.

## F4 — Merge, both kinds (native)
- [x] Cursor merge: both players get same new hue, inventory rows + XP —
      REASONED from `server/src/lib.rs` (`set_pos`'s nearest-partner scan +
      `apply_merge`, unchanged since F1). Client feedback added this batch:
      `main.rs` diffs `ctx.db.inventory()` each frame against a seeded
      `HashSet<u64>` of known ids; a new row owned by `me` calls
      `ui_state.show_merge_toast(hue, partner_label)`. Seeding waits for the
      caller's own starting `inventory` row to appear (guaranteed by
      `client_connected`) before diffing, so the pre-existing row never
      false-fires a toast on connect. Not yet run live.
- [x] Re-touch → no second merge; Lock blocks merge — REASONED, unchanged
      server logic (`me.hue != partner_hue` guard + `!other.locked` /
      `!me.locked` checks in `set_pos`). Not yet run live.
- [x] Long-press tile merge credits both sides — implemented: holding LMB
      steady (≤`LONG_PRESS_TOL_PX`=8px) for `LONG_PRESS_HOLD`=400ms calls
      `merge_with_cell(kind, id)` (server-side `apply_merge` still credits
      both `a`/caller and `b`/painter with an inventory row + XP if new for
      them — unchanged since F1). Target cell is snapshotted at press-time
      via new `main.rs` helpers `island_at` (like `classify` but checks every
      island, not just the caller's) and `merge_target_at` (resolves to an
      `island_cell` or `margin_cell` row). REASONED from code; not yet run
      live (needs two instances).
- [x] **DEVIATION (author-requested):** long-press no longer runs the blend
      formula — `merge_with_cell` in `server/src/lib.rs` now passes the
      tile's exact `tile_hue` straight to `apply_merge` instead of
      `merge::merge_hue(me.hue, tile_hue)`. Reason: repeatedly long-pressing
      the SAME placed tile used to keep producing a brand-new blended hue
      every time (since the caller's hue changes after each merge, the next
      blend against the same static tile is never equal to it), letting one
      tile be farmed for unlimited XP/colors. Taking the exact color instead
      means after the first take `tile_hue == me.hue`, so the very next
      press on that tile hits the existing "brush already matches this tile"
      guard and is a no-op — cursor-merge (touching another live cursor)
      still uses the blend formula and is the only way to create a hue that
      didn't already exist on the board. `cargo build -p server` clean;
      republished non-destructively; not yet re-run live.
- [x] 180°-apart hues deterministic (REASONED from formula) — `merge::merge_hue`
      (now only reachable via cursor-merge) special-cases `diff == 180` to
      `(h1.min(h2) + 90) % 360`, sidestepping the `atan2` singularity where
      both components cancel to `(0, 0)`.
- [x] **Addition (author-requested):** Hue slider in the inventory overlay
      (`ui.rs`), ±5° either side of the selected/anchor hue
      (`world::constants::HUE_TOLERANCE`, mirrored server-side as
      `constants::HUE_TOLERANCE`). Reason: unlocking an exact single degree
      felt too rigid — painting should be able to land on a nearby shade, not
      only the bit-exact merged/taken value. `set_brush`'s validation
      loosened from an exact-match check to `hue_dist(inv.hue, hue) <=
      HUE_TOLERANCE` (circular distance, handles the 359→0 wrap) so one
      unlock covers a small neighborhood instead of bloating the inventory
      with one row per nudge. The slider's anchor (`UiState::base_hue`) is
      set exactly on any swatch click and otherwise self-corrects each frame
      whenever the live brush hue drifts outside the ±5 window — which is
      how it picks up a merge (cursor-merge changes both players' hue, so
      both auto-recenter; tile-merge/eyedropper only changes the caller's, so
      only the caller recenters) without `main.rs` needing to know the slider
      exists. REASONED from code; not yet run live. Non-schema server change,
      republished with `--delete-data=on-conflict` (preserves existing local
      test data — the blanket `--delete-data=always` in `publish.sh` was
      intentionally NOT used this time).

Implementation notes:
- "Long-press on a cell you're NOT painting": rather than special-casing
  paintable-vs-not, the trigger condition is simply "the tile's hue still
  differs from your brush by the time the hold threshold fires". If the cell
  was paintable by you, the ordinary paint-on-press call already overwrote it
  with your own hue that same frame, so the snapshot naturally no longer
  differs and the client skips the call; if it wasn't paintable by you
  (someone else's island), the hue never changes and the merge fires. The
  server's own `tile_hue == me.hue` check in `merge_with_cell` is the
  authoritative backstop either way — a stale client snapshot can only cause
  a harmless rejected no-op call, never an incorrect merge.
- Hold-progress ring: `world::draw_hold_ring` (screen-space partial ring,
  raylib's `draw_ring` with `end_angle = 360 * frac`), drawn last in
  `main.rs` (topmost, same z-order reasoning as the own-cursor fix in F3).
- Toast: `ui::UiState` gained a `Toast { text, hue, shown_at }`, drawn by
  `draw_toast` just under the header for `TOAST_DURATION`=2.5s with a
  fading-alpha banner + flash swatch of the new hue at `sat_cap`/90 (not the
  live brush — the toast is about the newly *obtained* color, not the
  currently-selected one). `show_merge_toast` also calls `note_used_hue` so
  a merge-obtained color shows up in the footer's last-3 immediately.
- Partner label (`player_label` in `main.rs`) uses the partner's `name` if
  set, else falls back to their short identity hex (`short_hex`, factored
  out of the header-drawing logic in `ui.rs` — not touched there, just
  duplicated at the point of use since `ui.rs` has no `DbConnection` access).
- **Fix (author-requested):** long-pressing a color you already own is now a
  complete no-op — no reducer call, no hold-progress ring at all. Server:
  `merge_with_cell` in `server/src/lib.rs` gates on the caller's inventory
  already containing `tile_hue` (replacing the old `tile_hue == me.hue`
  check, which could miss an owned color if the brush had been Hue-slider-
  nudged a few degrees off it). Client: the new `have_hue()` helper in
  `main.rs` filters `LongPress.target` at press-time, so if you already have
  the hue nothing is snapshotted as a target and the ring never appears.
  Also added a "+" hover hint (`world::draw_plus_hint`, a small circled plus
  near the cursor) shown whenever the hovered cell is someone else's painted
  island tile with a hue you don't yet own — the only cells where long-press
  can actually succeed (your own paintable cells get overwritten by the
  ordinary paint-on-press before the hold timer fires, so they're never a
  real eyedropper target regardless of ownership). `cargo build -p server`
  and `-p client` (`client`, `bot`) clean; republished non-destructively
  again (`--delete-data=on-conflict`); not yet re-run live.
- **Fix (author-caught):** the inventory grid's gold "selected" border
  compared a swatch's hue to the LIVE brush hue (`info.brush.0`), so nudging
  the Hue slider away from 0 made every swatch look unselected even though
  you were still fine-tuning the same color. Now compares against
  `state.base_hue` (the slider's anchor, which only changes on an explicit
  swatch click or a merge) instead — `cargo build -p client --bin client`
  clean, not yet re-run live.
- Adding the Hue slider pushed the Saturation/Value sliders up 50px to make
  room (three stacked sliders instead of two, at overlay-height minus
  160/110/60). Same latent constraint as before, just tighter: the swatch
  grid and the slider block share the overlay's vertical space with no
  collision check, so somewhere around 6 rows (~60 unlocked hues at
  `SWATCH_COLS`=10) the grid would start growing into the sliders. Not hit in
  practice yet at jam timescales; flagging in case the author unlocks a lot
  of colors before F7.
- **Tweak (author-requested):** `swatch_color()` in `ui.rs` used to render
  only the currently-selected hue at the live brush sat/val, with every other
  swatch pinned to a static `(sat_cap, 90)` preview. Changed so ALL swatches
  (footer last-3 and the inventory grid) track the live sat/val sliders —
  dragging saturation/lightness now re-previews every unlocked hue at once,
  which is the point of comparing them before picking. `cargo build -p
  client --bin client` clean.
- **Fix (author-caught, three in one batch):**
  1. "First open of Colors, my color wasn't selected" — root cause: the
     Hue-slider resync in `ui::handle_input` snapped `base_hue` to the raw
     live brush value verbatim, which after any prior Hue-slider nudge (or
     across a client restart, since `user.hue` persists server-side) is
     usually NOT bit-exact to any owned inventory entry — so the "selected"
     compare (`hue == state.base_hue`) never matched anything. Fixed: resync
     now snaps to the NEAREST exact hue in `info.hues` instead
     (`min_by_key(|&h| world::hue_dist(info.brush.0, h))`), so `base_hue` is
     always a real owned entry.
  2. "Changing the hue by hand filled my last-3" — `note_used_hue` was called
     in `main.rs` on every `set_brush` action, including the ones fired
     continuously while dragging the Hue/Sat/Val sliders. Moved the call into
     `ui.rs` itself, only at the two explicit-click sites (last-3 rect,
     inventory swatch) — slider drags no longer touch last-3 at all.
  3. "Long-press still available on a tile at hue-5 after switching my brush
     to hue+5, even though it's the same color" — `have_hue` (client) and
     `merge_with_cell`'s ownership check (server) both used to compare EXACT
     hue equality against inventory, but inventory only ever stores the
     unnudged `base` value, never the transient nudged paint value. A tile
     painted at `base - 5` never exactly equals the owned `base` entry, so it
     was wrongly treated as "not owned". Both sides now use the same
     `hue_dist(...) <= HUE_TOLERANCE` window `set_brush` already validates
     against, promoted to a shared `world::hue_dist` (client) /
     already-existing `hue_dist` (server) so client and server can't
     disagree. `cargo build -p server`, `-p client` (`client`, `bot`), and
     `cargo build --workspace --exclude client` all clean; republished
     non-destructively again; smoke-tested connect. Not yet re-run live for
     the actual fixed behavior.
- **Fix (author-caught):** clicking a swatch caused a 1-frame slider glitch —
  the Hue handle would snap to an extreme (min or max) before settling at 0.
  Root cause: a click sets `state.base_hue` to the NEW hue immediately
  (local), but `info.brush.0` (server-confirmed) is still the OLD hue for
  that same frame, so `draw_overlay`'s offset computation
  (`hue_offset_signed(info.brush.0, state.base_hue)`) briefly saw a huge gap
  between two possibly-distant colors, which `clamp(-tol, tol)` forced to an
  endpoint until the `set_brush` round trip landed. The same staleness could
  also have made the resync check flicker `base_hue` back toward the old
  selection for a frame. Fixed with `UiState::pending_select`: an explicit
  click now also records the hue it just asked for; a new `effective_hue()`
  helper returns that pending value (which by construction already equals
  `base_hue`, so the offset is exactly 0) until `info.brush.0` confirms it,
  and both the resync check and the slider's offset display now go through
  it instead of raw `info.brush.0`. No server changes. `cargo build -p
  client` (`client`, `bot`) and `cargo build --workspace --exclude client`
  clean; smoke-tested connect. Not yet re-run live.

## F5 — Web client parity
- [x] Tables + reducers mirrored in `web.rs` / `game.html`; sessionStorage → localStorage —
      VERIFIED (build) / REASONED (behavior). `web.rs` fully rewritten: generic
      `RowView` (named-object or positional-array) + `identity_hex`/
      `timestamp_micros`/`opt_value` helpers parse every table row once, instead
      of the old per-table copy-pasted parsers; `Tables` struct holds
      `HashMap`s for `user`/`inventory`/`island`/`island_cell`/`margin_cell`
      (config is subscribed to, per plan.md, but has no client-side consumer
      yet — nothing to parse). All P0 reducers wired: `set_pos`, `set_name`,
      `set_lock`, `set_brush`, `paint_island_cell`, `paint_margin_cell`,
      `merge_with_cell`, via a single `call_reducer(name, args: Value)`
      helper. `cargo check --target wasm32-unknown-emscripten` clean.
- [x] **Fix/hardening beyond the old web client:** the old `web.rs` built each
      `callReducer` JS call by directly interpolating the JSON args into a
      single-quoted JS string literal (`format!("...'[{}]'...", x)`) — fine for
      pure numbers, but `set_name` now sends arbitrary player-typed text, and an
      apostrophe or backslash in a name would have broken out of the JS string
      literal (self-XSS risk, at minimum a crash). `call_reducer` now runs BOTH
      the reducer name and the JSON args string through `serde_json::to_string`
      before splicing them into the `run_js` source, so the result is always a
      valid, safely-escaped JS string literal regardless of content. REASONED
      from code (JSON string syntax is a valid subset of JS string syntax);
      not yet exercised with an actual apostrophe-containing name live.
- [x] Render/camera/input ported from `main.rs`, reusing `world.rs`/`ui.rs`
      unchanged (no per-platform fork of geometry or HUD code) — REASONED,
      structurally identical to `main.rs`'s frame loop with `ctx.db.*`/
      `ctx.reducers.*` replaced by the local `Tables` lookups /
      `call_reducer`. Other players' cursors are drawn directly from
      `user.cx/cy` each frame (no interpolation/lerp) to match native's
      actual behavior — the old pre-F1 web client had its own lerp/smoothing
      layer that native never had; dropped for real parity rather than kept
      as an unrequested nicety.
- [x] `./build-web.sh` passes (release, as required — debug still crashes the
      emscripten linker per the existing note) — VERIFIED: builds clean,
      `client/web/{web.js,web.wasm}` produced.
- [ ] Dual-iframe page: paint + merge across iframes — BLOCKED, no browser
      available to me; author to hand-test. `index.html`'s two iframes now
      get distinct identities via `game.html?slot=1|2` → distinct
      `localStorage` keys (`stdb_token_slot1`/`stdb_token_slot2`), replacing
      the old sessionStorage-based isolation (which localStorage cannot
      reproduce — same-origin iframes always share one `localStorage`).
- [ ] Phone: pinch zoom, two-finger pan, paint, long-press merge — BLOCKED, no
      phone/browser available to me. Implemented via raylib's own
      `get_touch_point_count`/`get_touch_position` (cross-platform touch API,
      no custom JS gesture code needed): one-finger tap/drag/hold is assumed
      to already arrive as ordinary mouse events via raylib's web/GLFW
      backend (ASSUMED — this is how the OLD web client's paint-by-drag
      worked without any touch-specific code, but not independently
      re-verified here) so it reuses the mouse code paths unchanged; two-
      finger gestures use a `Pinch` anchor (world point under the two-finger
      midpoint at gesture start stays pinned under the current midpoint each
      frame — pan and zoom both fall out of that one invariant rather than
      separate delta bookkeeping) — REASONED from code, not run on real
      touch hardware.
- [x] `du -sh client/web` well under 64 MB — VERIFIED: 880K total
      (`index.html` 4K, `game.html` 8K, `web.js` 224K, `web.wasm` 644K).

## F6 — Identity: token login, reset
- [x] Copy ID (with selectable-text fallback), paste-token import in a private window —
      IMPLEMENTED (Account overlay, `ui.rs`/`game.html`/`bin/web.rs`/`main.rs`), REASONED
      from code; not yet run live in a browser (needs author hand-test per plan.md's
      verify checklist: note identity, clear tab, reopen, copy token, import in a
      private window).
- [ ] Refresh/reopen keeps identity; `SELECT COUNT(*) FROM user` unchanged —
      REASONED (unchanged since F5's localStorage persistence); BLOCKED on author
      hand-test, same as above.
- [x] Reset: 1 fresh hue, XP 0, island art intact (author to confirm art-survives rule) —
      `reset_account` (`server/src/lib.rs`) deletes all inventory rows, inserts one
      fresh hue, zeroes xp, resets sat/val to defaults, keeps `name`/`identity`/island
      ownership (island_cell rows are never touched by this reducer). **Bug fixed this
      batch** (author-caught via `known_bugs.md`): the fresh hue used to come from
      `start_hue(identity)`, deterministic per identity, so a reset — which keeps the
      SAME identity by design — always produced the SAME hue as before, not a new one.
      Now uses `ctx.rng().gen_range(0..360u16)`. REASONED from code; BLOCKED on author
      hand-test to confirm island art survives in practice and the hue is now actually
      different each reset.

## F7 — Deploy + itch.io package  ← game is submittable when this is done
- [ ] Host-resolution rule (localhost/private-IP → ws local, else wss production) —
- [ ] VPS: module published, web rebuilt, old bots stopped —
- [ ] itch DRAFT tested end-to-end (two devices, wss, touch) —
- [ ] Identity survives itch page reload —
- [ ] Zip < 64 MB, 720×720 exact, fullscreen + mobile flags set —
- [ ] SUBMITTED (then redeploys until 2026-07-12 18:00 UTC) —

---

## P1 (only after F7)
- [x] F8 — likes, island info popup, 5-min re-rank + countdown — server
      logic VERIFIED live (`like_island`/`unlike_island` incl.
      self-like/unknown-island rejection + XP credit/revert; full two-step
      rerank chain; an ACTUAL slot permutation under a manufactured ranking
      change — see the batch notes above). Client (popup, Like/Unlike
      toggle, double-click-to-like gesture, own-island border color/width,
      "My Isle" footer button, countdown banner) VERIFIED — author
      hand-tested across three feedback rounds and confirmed it feels good.
      Builds all clean, package size fine. COMMITTED.
- [ ] F9 — island links (jam rate id only), link-click XP, time XP —
      implemented (see the batch notes above); server-side dedupe/guards
      VERIFIED via CLI, client wiring + the 60s time-XP tick REASONED only
      — awaiting the author's hand-test before checking this off —

## P2 (only if time remains)
- [ ] F10 admin — [ ] F11 flying gift — [ ] F12 polish/bots/sounds —
- [ ] F13 hexa event (6-cursor hexagon: pooled dictionaries, one-time XP, snap rendering) —

## Notes / deviations from plan.md
- **New at F6 (author-directed, not in plan.md's original text):** players
  get an auto-generated random name (adjective+noun, e.g. "SwiftFox") at
  first connect instead of `name: None`. Motivated by a privacy fix, not a
  cosmetic one — see the F6 batch note above and decision 11 (the identity
  doubles as the account-recovery token, so it must never leak, not even
  truncated, and the merge toast's old no-name fallback was doing exactly
  that). Players can still rename via the existing `set_name` UI at any time.
- `PAINT_BUCKET_MAX`/`PAINT_REFILL_PER_SEC` raised, in two live-tuning passes
  during F2 hand-testing, to 1000/50 (50x the original 20/1.0); `plan.md`'s
  canonical constants table updated to match (same ratio, same "N tiles /
  20s" shape). Client's `CLIENT_PAINT_HZ` throttle raised to 100 to match so
  it doesn't become the new bottleneck under the server's bucket. Author
  edited `server/src/lib.rs`/`client/src/main.rs` directly for the second
  pass (20→100 to 1000/50 and 75→100); values above reflect that.
- `libm = "0.2"` added to `server/Cargo.toml` — wasm32-unknown-unknown's `std` does
  not link `sin`/`cos`/`atan2`; needed for the hue-merge circular-mean formula.
- CLI version: **RESOLVED**. The `~/.local/bin/spacetime` v2.7.0 found during F1
  was an unrelated stray install; plan.md's pin to v2.6.1 was correct all along
  (the official install script installs 2.6.1). Author reinstalled 2.6.1
  locally. All F1 verification above was done against 2.7.0 and should be
  treated as REASONED-not-VERIFIED until re-run against 2.6.1 (re-publish +
  regenerate bindings once the local instance is back up on 2.6.1; the schema
  and reducer code itself doesn't change, only the toolchain).
- `client/src/bin/web.rs` (and therefore `./build-web.sh`) does not compile as
  of F1: it still uses the pre-F1 schema (`Cell` table, `paint_cell` reducer,
  pixel x/y coords), all removed in the rewrite. Deleting the also-superseded
  `hexgrid.rs` (folded into `world.rs` in F2) surfaces this as a `mod hexgrid`
  file-not-found error, but the real breakage is the schema mismatch and
  predates that deletion. **Decision (author, when asked):** extend the same
  exception plan.md already grants `bot.rs` — `web.rs`/`./build-web.sh` is
  allowed to not compile until F5 migrates it to the new schema; this is not
  a P0 blocker and is not attempted early. Tracked here so it isn't mistaken
  for a regression at each feature boundary's build check.
  **RESOLVED at F5**: `web.rs` rewritten against the current schema; `./build-web.sh`
  builds clean again.
- **SpacetimeDB 2.6.1 `v1.json.spacetimedb` wire format**, verified live via a
  throwaway WebSocket probe (F5) rather than the docs — recorded here since
  nothing else pins it down and the next person touching the wire layer will
  need it: row payloads inside `updates[].inserts`/`.deletes` are JSON
  *strings* (double-encoded) in both `InitialSubscription` (named-object rows)
  and `TransactionUpdate`/`TransactionUpdateLight` (positional-array rows,
  schema field order). `Identity` is `{"__identity__": "0x.."}` named or a
  nested one-element array `["0x.."]` positional. `Timestamp` is
  `{"__timestamp_micros_since_unix_epoch__": N}` named or `[N]` positional.
  `Option<T>` — including inside a NAMED row, which was the surprising part —
  is uniformly a two-element array `[tag, payload]`: tag `0` = `Some(payload)`,
  tag `1` = `None` (payload is a throwaway `{}`/`[]`). A rejected `CallReducer`
  (e.g. an unknown reducer name, a reducer erroring out) comes back as
  `TransactionUpdate.status: {"Failed": "<message>"}` instead of
  `{"Committed": {...}}` — `web.rs` simply has no `database_update` to apply
  in that case, matching how `ctx.reducers.*`'s `Result<(), String>` errors
  are already silently `let _ =`-discarded on the native side.
- `slot_coords`/`occupied_rings`/margin-bound math (geometry spec's "occupied
  rings" wording) is my interpretation, not explicitly pinned down by plan.md:
  `occupied_rings(n)` = the ring index containing the `n`-th (0-indexed) slot.
  Flag to the author if the canvas bound should grow differently.
- `merge_with_cell`'s cell-color → hue unpack masks with `0x1FF` after the `>>16`
  shift; redundant given the packing layout (harmless, kept for clarity of intent
  that hue is 9 bits) — not a bug, just noting it's belt-and-suspenders.
- **Design ruling superseding F4's margin-cell long-press support**: F4's
  `merge_target_at` (see its notes above, "resolves to an `island_cell` or
  `margin_cell` row") and F1's `merge_with_cell` originally accepted
  `cell_kind == 1` (margin). Post-F5 hand-testing found this broken in
  practice (ordinary paint-on-press overwrites a margin tile before a
  long-press can fire) and the author ruled it out entirely rather than
  patching around it — see the batch note at the top of this file. Margin is
  no longer a long-press-merge target in either client or the server.
