# Hexaworld — execution status

Companion to `plan.md`. The executor updates this file after every batch (1–2 tasks).

Evidence tags (mandatory on every checked item):
- `VERIFIED` — ran the command / exercised the behavior and inspected the output.
- `REASONED` — read the code and traced the logic visually.
- `ASSUMED` — unchecked hypothesis; must be verified before the next batch starts.

**Current batch:** F4 implemented (merge toast/flash feedback, long-press
tile-merge with hold-progress ring), plus two author-requested design changes
on top: long-press now takes the tile's exact color instead of blending
(fixes an exploit where the same placed tile could be merged against
repeatedly for free XP/colors), and a Hue slider (±5°) so painted/unlocked
shades don't have to be bit-exact. `cargo build -p client --bin client`,
`--bin bot`, `cargo build --workspace --exclude client`, and `cargo build -p
server` all clean; smoke-tested `cargo run -p client --bin client` against
the local instance (connects and renders, no crash). Server republished
non-destructively (`--delete-data=on-conflict`, not `publish.sh`'s
`always` — no schema changed, so existing test data/islands survived) and
bindings regenerated. Since then, three more author-caught fixes in the same
area (see F4 notes below): "selected" swatch missing on first Colors open
(anchor resync now snaps to the nearest OWNED exact hue, not the raw nudged
value), Hue-slider drags were polluting last-3 (now only explicit swatch
clicks note a used hue), and the already-have-color check for long-press
used exact hue equality instead of the same ±5 tolerance `set_brush` allows
(now shared via `world::hue_dist`). One more after that: a 1-frame slider
glitch on swatch click (handle snapping to min/max before settling at 0),
fixed with `UiState::pending_select` (see F4 notes below). All rebuilt
clean, re-republished non-destructively, author confirmed "clean and
commit" — this batch is now committed.
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
- [ ] Tables + reducers mirrored in `web.rs` / `game.html`; sessionStorage → localStorage —
- [ ] `./build-web.sh` passes; dual-iframe page: paint + merge across iframes —
- [ ] Phone: pinch zoom, two-finger pan, paint, long-press merge —
- [ ] `du -sh client/web` well under 64 MB —

## F6 — Identity: token login, reset
- [ ] Copy ID (with selectable-text fallback), paste-token import in a private window —
- [ ] Refresh/reopen keeps identity; `SELECT COUNT(*) FROM user` unchanged —
- [ ] Reset: 1 fresh hue, XP 0, island art intact (author to confirm art-survives rule) —

## F7 — Deploy + itch.io package  ← game is submittable when this is done
- [ ] Host-resolution rule (localhost/private-IP → ws local, else wss production) —
- [ ] VPS: module published, web rebuilt, old bots stopped —
- [ ] itch DRAFT tested end-to-end (two devices, wss, touch) —
- [ ] Identity survives itch page reload —
- [ ] Zip < 64 MB, 720×720 exact, fullscreen + mobile flags set —
- [ ] SUBMITTED (then redeploys until 2026-07-12 18:00 UTC) —

---

## P1 (only after F7)
- [ ] F8 — likes, island info popup, 5-min re-rank + countdown —
- [ ] F9 — island links (jam rate id only), link-click XP, time XP —

## P2 (only if time remains)
- [ ] F10 admin — [ ] F11 flying gift — [ ] F12 polish/bots/sounds —
- [ ] F13 hexa event (6-cursor hexagon: pooled dictionaries, one-time XP, snap rendering) —

## Notes / deviations from plan.md
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
- `slot_coords`/`occupied_rings`/margin-bound math (geometry spec's "occupied
  rings" wording) is my interpretation, not explicitly pinned down by plan.md:
  `occupied_rings(n)` = the ring index containing the `n`-th (0-indexed) slot.
  Flag to the author if the canvas bound should grow differently.
- `merge_with_cell`'s cell-color → hue unpack masks with `0x1FF` after the `>>16`
  shift; redundant given the packing layout (harmless, kept for clarity of intent
  that hue is 9 bits) — not a bug, just noting it's belt-and-suspenders.
