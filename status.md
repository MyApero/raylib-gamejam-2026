# Hexaworld — execution status

Companion to `plan.md`. The executor updates this file after every batch (1–2 tasks).

Evidence tags (mandatory on every checked item):
- `VERIFIED` — ran the command / exercised the behavior and inspected the output.
- `REASONED` — read the code and traced the logic visually.
- `ASSUMED` — unchecked hypothesis; must be verified before the next batch starts.

**Current batch:** F3 (`ui.rs` header/footer/inventory overlay) implemented,
builds clean; not yet hand-tested by the author.
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
      count), 44px footer (center-on-island button, last-3-hue swatches,
      inventory toggle, name text field, Lock toggle), modal inventory
      overlay (unlocked-hue grid at `sat_cap`, saturation slider capped at
      `SAT_CAP(level)`, value/luminosity slider 0–100 uncapped) — REASONED
      (fits 720×720 by construction: all rects hand-placed against the fixed
      720 screen size; overlay is modal via `map_input_allowed =
      !ui_state.overlay_open` gating zoom/pan/cursor-heartbeat/painting in
      `main.rs`, checked by inspection, not yet run). `cargo build -p client
      --bin client` clean, no warnings.
- [x] `set_brush`/`set_name`/`set_lock` wired from `ui::Actions` in
      `main.rs` — REASONED from code, not yet exercised live. Sat slider's
      `max` argument is `info.sat_cap` (`world::sat_cap(level)`), so it's
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
- [ ] Cursor merge: both players get same new hue, inventory rows + XP (sql check) —
- [ ] Re-touch → no second merge; Lock blocks merge —
- [ ] Long-press tile merge credits both sides —
- [ ] 180°-apart hues deterministic (VERIFIED or REASONED from formula) —

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
