# Hexaworld — execution status

Companion to `plan.md`. The executor updates this file after every batch (1–2 tasks).

Evidence tags (mandatory on every checked item):
- `VERIFIED` — ran the command / exercised the behavior and inspected the output.
- `REASONED` — read the code and traced the logic visually.
- `ASSUMED` — unchecked hypothesis; must be verified before the next batch starts.

**Current batch:** F13 Hexa event — author follow-up (2026-07-11, same day),
superseding two of the first pass's rendering design calls. Author feedback
verbatim: "I'm expecting my cursor to literally go at that place when doing
HEXA" and "it would be rendered by the client but the server would tell
that there is an HEXA happening and give an id and position to players so
that everyone see they are merging." Both addressed by moving cluster
DETECTION server-side (previously client-side-only, an approximate
connected-components guess) and having the local player's own cursor snap
too (previously excluded, glued to the literal mouse position).

- **Server** (`server/src/lib.rs`): new `hexa_cluster(identity pk,
  cluster_id, cx, cy, member_count, vertex_index, ignited)` table, `public`.
  `set_pos`'s existing per-caller detection scan (already needed for the
  ignition trigger) now also collects each eligible member's POSITION (not
  just identity), and — whenever the caller's own cluster has >= 2 members —
  calls a new `upsert_hexa_cluster` helper that writes/updates EVERY
  member's own row in one pass, not just the caller's, so a single mover's
  `set_pos` call refreshes the whole visible group at once rather than
  waiting for each member to happen to move themselves. `cluster_id` is a
  hash of the sorted member identities (`hexa_cluster_id`, `DefaultHasher`
  over `Identity`'s `Hash` impl) — deliberately NOT an arbitrary counter, so
  independent callers' own scans naturally agree on the same id as long as
  they detect the same membership (no cross-call synchronization needed),
  and the id changes the instant membership actually changes (verified live,
  see below). `vertex_index` is each member's position in that same sorted
  order — clients read it directly instead of re-deriving vertex assignment
  themselves. A caller whose own cluster drops below 2 members deletes their
  own row. The existing `>= HEXA_SIZE` ignition/reward path (`apply_hexa`)
  is UNCHANGED, just fed from the same already-computed member list.
  New repeating `hexa_sweep` tick (`HEXA_SWEEP_PERIOD_SECS`, 2s, private
  `hexa_sweep_schedule` table, same lazy-seed pattern as every other
  schedule) deletes any `hexa_cluster` row whose owner has gone offline or
  stale — a departing/disconnecting member might never call `set_pos` again
  to clear their own row, which would otherwise linger showing a hexagon
  that no longer really exists.
- **Both clients** (`world.rs`, `main.rs`, `bin/web.rs`, `game.html`):
  dropped the entire client-side clustering algorithm (`world::
  hexa_clusters`/`HexaCandidate`, an undirected-graph connected-components
  pass over guessed candidates) — no longer needed now that the server is
  authoritative. Both clients now group `hexa_cluster` rows by `cluster_id`
  (pairing each row with its OWN `vertex_index`-derived position, not by
  list order, so a momentarily incomplete subscription snapshot can't
  misassign slots) and feed that straight into the unchanged
  `hexagon_vertex_positions`/`hexa_advance_display`/`draw_hexa_polygon`
  render helpers. New `"SELECT * FROM hexa_cluster"` subscription (native
  list + `game.html`). Local-cursor snap: both `main.rs` and `bin/web.rs`
  precompute a `my_hexa_screen: Option<Vector2>` (same "must compute before
  `begin_drawing` borrows `rl`" constraint `other_cursors` already has) from
  `hexa_display.get(&me)`, and the local player's own `draw_cursor` call now
  uses `my_hexa_screen.unwrap_or(mouse_screen)` instead of always
  `mouse_screen` — painting/hover logic elsewhere is UNCHANGED, still reads
  the real mouse position; only this one draw call's position moves.
- **Verify — server, VERIFIED live**: re-ran a 6-identity SDK probe
  (`client/src/bin/hexa_probe.rs`, deleted after use, third throwaway probe
  this project — same precedent as F11/F13's first pass) against the
  reworked schema. Clustering all 6 (reusing the same probe identities/hues
  from the first pass's verification, `--delete-data=never` having kept
  them around) showed identical `cluster_id` and stable, distinct
  `vertex_index` values (0..5) across every member's own row, `ignited:
  true` on all 6 the instant `member_count` hit 6. Walked one identity far
  away (500, 500) and nudged a remaining member: the departed identity's own
  row was deleted immediately; the other 5's rows refreshed to
  `member_count: 5`, `ignited: false`, and — confirming the hash-based id
  isn't just a counter — a NEW `cluster_id` (genuinely different membership,
  genuinely different hash). Left the group idle (no further `set_pos` from
  anyone) for ~6s: `hexa_sweep` cleared every remaining row once each
  member's `last_seen` went stale past `PRESENCE_TIMEOUT_SECS` —
  `spacetime sql`'s `hexa_cluster` table was empty afterward, confirming the
  safety net (a real player's client sends `set_pos` continuously while
  the mouse moves, so this only ever fires for genuinely idle/gone players).
- **Verify — client, REASONED, not hand-tested**: `cargo check -p server`,
  `cargo build -p client --bin client --bin bot`, and `./build-web.sh` all
  clean (zero warnings) after the rework, including alongside F14's
  concurrently-landed community-island changes (both present in the working
  tree by the time these builds ran). The local-cursor snap and
  hexa_cluster-row grouping logic itself is REASONED from the code, not run
  in the GUI — per this repo's established protocol the author drives
  actual client hand-testing.

**Verify status:** server mechanic (live cluster broadcast, id stability,
shrink-on-departure, sweep cleanup) VERIFIED live; both clients' rendering
(including the new local-cursor snap) REASONED only. NOT committed — full
author hand-test pass pending, same as every prior batch, PLUS this is the
piece that most needs an actual GUI look (does the local cursor snapping
away from the literal mouse position feel right, or disorienting, while
painting/hovering nearby is still mouse-driven underneath it).

---

**Previous batch:** F14 Community island (playground) — author ruling
2026-07-11 (plan.md decision 20), resolving the backlog's "center-island
identity" question: "the middle island should be a playground where anyone
can draw anything." Slot 0 is now a permanent, ownerless canvas open to
every connected player, and `claim_admin` no longer relocates any island
to it.

- **Server** (`server/src/lib.rs`): `client_connected` lazy-seeds an `Island`
  row at `slot == 0` if missing — same lazy-seeding convention already used
  there for `config` and the four scheduled-reducer rows, chosen specifically
  because a `--delete-data=never` republish of an EXISTING database (this
  one) does not re-run `init`, so an `init`-only approach would never have
  backfilled it here. Owner is `Identity::ZERO`, a real constant on the SDK's
  `Identity` type — chosen over `Option<Identity>` to avoid changing the
  `owner` column's type (and every one of the ~10 call sites that read it)
  everywhere it's already used; `Identity::ZERO` can never be a real
  connecting player's identity, and `lowest_free_slot` already starts its
  scan at 1, so it was never at risk of being handed to a real player either
  way. Two new reducers mirror `paint_island_cell`/`erase_island_cell`
  exactly (same `hexdist <= ISLAND_RADIUS` bound, same `take_paint_token`
  charge) but look the island up by `slot().find(0)` instead of
  `owner().find(ctx.sender())`, and have NO ownership check — any connected
  caller may write. `claim_admin`'s island-relocation block (the
  bump-through-a-temporary-slot swap) was deleted entirely; it now only
  checks the password and sets `config.admin` — admin is purely a role
  (freeze/`delete_island_cells` powers) with no physical placement anymore.
- **Clients** (`main.rs` native, `bin/web.rs` web — mirrored identically):
  `classify`/`Paintable` gained a third case, checked after "own island" and
  before "margin": inside slot 0's territory (always the world origin, a
  fixed constant — no island lookup needed to classify it) dispatches to
  `paint_community_cell`/`erase_community_cell` via a new `kind = 2` in the
  existing `(kind, q, r)` dispatch tuple. The foreign-island info-popup/
  hover-tooltip (decision 17, F9.5 item 7) opens for the community island
  exactly like any other foreign island (see the naming follow-up below for
  what it displays). Long-press tile-merge and the middle-click eyedropper
  needed NO changes — both already operate generically on any painted cell
  via `island_at`, which the community island's real (if ownerless) `Island`
  row satisfies for free.
- **Follow-up, same session (author-requested): naming + white background.**
  `player_label` (both clients) now special-cases the sentinel identity
  (native: `id == Identity::ZERO`; web: a new `COMMUNITY_OWNER_HEX` constant,
  the 64-zero hex form matching `normalize_identity`'s lowercase-no-`0x`
  output) to return `"Free Isle"` instead of falling through to the generic
  "another player" — this is the only place `island.owner`'s label was ever
  rendered (the info popup's "Owner: X" line), so the earlier design call to
  exclude the community island from that popup entirely was reverted: it now
  opens on hover/click like any other foreign island, just labeled "Free
  Isle". Unpainted community-island tiles now render pure white
  (`Color::new(255,255,255,255)`) instead of the usual `(60,60,68)` gray
  placeholder, in both clients' per-cell fill lookup — a visible tell even
  before anyone's painted on it. Liking the Free Isle / its (never-set)
  itch link are left as harmless no-ops (already gracefully handled
  server-side since `Identity::ZERO` has no `user` row to credit) — not
  specifically disabled, since that wasn't asked for.
- **Verify — server, VERIFIED live**: a throwaway 2-identity SDK probe
  (`client/src/bin/community_probe.rs`, same precedent as F11/F13's deleted
  probes, deleted after use) connected two fresh identities (neither owning
  any island at slot 0). `spacetime sql` confirmed: the community `Island`
  row exists at `slot = 0` with `owner =
  0x0000...0000` (64 zeros); identity A's `paint_community_cell(3, -2)`
  landed a cell; identity B's `paint_community_cell(4, -2)` landed a
  *different* cell under B's own `painted_by`, proving a non-owner can
  paint; B's `erase_community_cell(3, -2)` then deleted A's cell — proving
  erasing isn't ownership-gated either, matching decision 18's "permitted
  exactly where painting is permitted"; identity A's out-of-bounds
  `paint_community_cell(50, 50)` produced no row (bound check rejected it,
  as `paint_island_cell`'s already does). `claim_admin`'s relocation removal
  is REASONED only (code inspection — the block is gone, `cargo check`
  passes; the live password's plaintext isn't available to verify the
  reducer end-to-end, only its SHA-256 is ever committed, by design).
- **Verify — client, REASONED, not hand-tested**: `cargo build -p server`,
  `cargo build -p client --bin client --bin bot`, and `./build-web.sh` all
  clean (re-confirmed after the naming/white-fill follow-up too). Rendering/
  input behavior (the new dispatch branch, the popup now opening with the
  "Free Isle" label, the white unpainted fill) is REASONED from the code,
  not run in the GUI — per this repo's established protocol the author
  drives actual client hand-testing.

**Verify status:** server mechanic (lazy-seed, paint/erase by non-owners,
bound check, admin decoupling) VERIFIED live; client dispatch/gating
REASONED only. NOT committed — full author hand-test pass pending, same
convention as every prior batch. Given the freeze window starts
2026-07-12 18:00 UTC, this needs a hand-test-and-redeploy pass before then
to count as shipped; otherwise it waits for after voting ends per the
freeze rule.

---

**Previous batch:** F13 Hexa event (P2, last plan.md feature) — the
6-same-hue-cursor mechanic plan.md already fully spec'd (unlike F11, this
wasn't a one-liner the executor had to flesh out). Implemented server
schema + trigger + effect, both clients' rendering, and verified the
server-side mechanic live.

- **Shared constants** (`shared/src/lib.rs`): `HEXA_SIZE` (6), `HEXA_RADIUS`
  (2.0 world units), `XP_HEXA` (150) — matches plan.md's canonical constants
  table exactly (already had the right values pre-filled).
- **Server** (`server/src/lib.rs`): two new tables — `HexaReward(identity pk,
  at)`, server-internal (not `public`, same treatment as a schedule table —
  clients only ever see its effect through `User.xp`); `HexaEvent(id
  auto_inc, at, cx, cy, member_count)`, `public`, for the client ignition
  animation. Trigger lives in `set_pos`, AFTER the existing pairwise-merge
  block (re-fetches the caller's `User` row first, since that block may have
  just changed the caller's own hue) — scans for online/fresh/unlocked users
  within `HEXA_RADIUS` whose hue is within `HUE_TOLERANCE` of the caller's,
  same shape as the existing pairwise scan just above it. At `cluster.len()
  >= HEXA_SIZE` calls a new `apply_hexa` helper (next to `apply_merge`):
  unions every participant's owned hues, grants whoever's missing one a new
  `Inventory` row (`obtained_with: None`, same as a seed/reset row — see the
  join note below), grants `XP_HEXA` once per identity ever (gated on a
  `HexaReward` row existing), and only inserts a `HexaEvent` row if that pass
  actually granted something new. Design call (not explicit in plan.md):
  gating the event-log insert on "something changed" reuses the same
  idempotence plan.md already grants the pooling effect — without it, a
  cluster that stays formed would insert a fresh `HexaEvent` row (and so
  re-trigger the clients' flash) on every single `set_pos` tick it holds.
  No hard cap at exactly 6 participants — a same-hue cluster bigger than
  `HEXA_SIZE` pools among all of them, not just the nearest 6 (flagged in
  plan.md as the executor's read of the author's "among the 6 only" ruling,
  which was about never pooling server-wide, not about a hard cap).
- **`Inventory` grant identification**: a Hexa-pooled row and a
  `reset_account` reseed both leave `obtained_with: None`, so they need
  telling apart. Per plan.md's own spec, NOT a new bool field (unlike
  F11's `from_gift`) — instead both `main.rs` and `bin/web.rs`'s existing
  inventory-insert watch check whether any `hexa_event` row's `at` exactly
  matches the new row's `obtained_at` (same `ctx.timestamp`, written in the
  same reducer call, so it's an exact match, not a tolerance window) before
  falling back to the reset-hue path. `bin/web.rs`'s hand-rolled
  `InventoryRow` had to grow an `obtained_at_micros` field for this — every
  other consumer of that struct had skipped `obtained_at` until now.
- **Client rendering** (`world.rs`, shared by both binaries): `HexaCandidate<K>`
  (generic over identity type — native's SDK `Identity`, web's hex
  `String` — so this module stays usable by both) + `hexa_clusters`, an
  undirected-graph connected-components pass over ALL present online+
  unlocked cursors. Deliberately a DIFFERENT, looser algorithm than the
  server's caller-centered trigger scan — rendering has no single "caller"
  to center on, and it skips the `PRESENCE_TIMEOUT` freshness check (never
  mirrored client-side; plain cursor rendering already made that same call,
  see `draw_cursor_label`'s "stay visible the whole time online" precedent)
  — so it's a display-only approximation of what the server actually
  ignited on, not a re-derivation of it. `hexagon_vertex_positions` (new
  client-only `HEXA_VERTEX_RADIUS` constant, not shared — the server has no
  vertex-layout concept) + `hexa_advance_display` (frame-rate-independent
  exponential lerp toward the current target, new client-only
  `HEXA_SNAP_LERP_SECS`; drops any key no longer clustered instead of
  lerping it back to nothing, so the persisted map never grows past however
  many cursors are hexagon-snapped right now) + `draw_hexa_polygon` (open
  chain below `HEXA_SIZE` members, closed bright hexagon at/above it — "flash"
  is a sustained bright/thick state, not a timed pulse). Wired into both
  `main.rs` and `bin/web.rs` identically: candidates include `me` (for
  correct centroid math) but only OTHER players' cursors get their render
  position overridden — the local player's own cursor stays glued to the
  literal mouse pointer, unchanged. New `UiState::show_hexa_toast` (`ui.rs`,
  shared) reuses the existing merge sfx cue rather than a new sound asset.
  `"SELECT * FROM hexa_event"` added to both the native subscription list
  and `game.html`'s (`hexa_reward` is not public, so nothing to subscribe
  to there).
- **Verify — server, VERIFIED live**: a throwaway 6-identity SDK probe
  (`client/src/bin/hexa_probe.rs`, same precedent as F11's throwaway
  WebSocket probe, deleted after use) connected 6 distinct identities,
  homogenized them to the exact same live brush hue via a legitimate
  tile-merge/eyedropper off one shared painted cell (avoids needing 6
  colliding random seed hues), clustered them at the world origin, and
  fired `set_pos` in sequence. The 6th call's own trigger scan saw all 6
  fresh + same-hue + close and ignited. `spacetime sql` against the local
  instance confirmed: exactly 6 `hexa_reward` rows (one per identity) and
  exactly 1 `hexa_event` row (`member_count: 6`); every participant's
  inventory grew to 6 rows (the union of the 6 distinct seed hues + the one
  shared eyedropped hue); XP matched `XP_MERGE_NEW` (25, from the
  eyedropper) + `XP_HEXA` (150) exactly for the 5 identities that eyedropped,
  and just `XP_HEXA` for the seed identity. Re-triggered `set_pos` on the
  still-formed cluster afterward: `hexa_reward` row count and every
  participant's `xp` were unchanged, and `hexa_event` stayed at exactly 1
  row — confirms the idempotence gate (both the "no new grants" AND "no
  event spam" halves of it).
- **Verify — client, REASONED, not hand-tested**: `cargo build -p server -p
  client --bin client --bin bot` and `./build-web.sh` all clean (no
  warnings after trimming `bin/web.rs`'s hand-rolled `HexaEventRow` down to
  the one field it actually reads). Rendering/gesture behavior itself is
  REASONED (traced the logic, not run in the GUI) — per this repo's
  established protocol the author drives actual client hand-testing, not
  the executor.

**Verify status:** server mechanic (trigger/pooling/one-time-XP/idempotence)
VERIFIED live; both clients' rendering REASONED only. NOT committed — full
author hand-test pass pending, same convention as every prior batch (F13
also specifically needs the author to actually SEE 6 cursors converge and
confirm the hexagon-snap rendering reads correctly, which no amount of
`spacetime sql` can substitute for).

---

**Previous batch:** F12 Polish, part 4/4 (last part) — itch page styling.
plan.md's spec: "page styling on itch." Unlike the other three parts, this
one isn't code — it's content on the itch.io project page itself, which
lives outside the repo and needs the author's itch.io login (this
environment has none). Drafted the actual page copy as `itch-page.md`
(new, repo root): pitch, "the idea" (explains merge in player-facing
language, reusing the same framing as F12 part 2's in-game help overlay so
the two stay consistent), a trimmed controls list, the "also playable at
raylib.mister-esman.uk" link plan.md's F7 already calls for, and a
tech/credits line. Also left author-facing notes at the bottom of that file
(screenshots/GIF of an actual merge, itch cover-image sizing, upload
settings per F7) since a page with copy but no images still undersells the
game on a jam listing page dominated by thumbnails.

**Verify status:** N/A — no code changed, nothing to build. This is
explicitly a draft for the author to paste into itch.io's editor (which
does its own formatting, not Markdown) and finish with real screenshots;
not something the executor can complete end-to-end without itch.io access.

With this, all four F12 sub-parts have been addressed (bots, help overlay,
sounds fully implemented; itch styling drafted pending the author). See the
checklist entry below for the overall NOT committed / pending-hand-test
status — nothing in this batch or the prior three has been hand-tested or
committed yet.

---

**Previous batch:** F12 Polish, part 3/4 — sound effects. plan.md's spec:
"sounds (raylib `LoadSound`, CC0 assets only)".

- **Assets**: raylib's own bundled `examples/audio/resources/` sfx
  (`coin.wav`/`spring.wav`/`sound.wav`/`weird.wav`), all CC0, made by
  raysan5 with rFXGen — copied into `client/assets/sfx/{merge,gift,levelup,
  error}.wav` (mapping + license documented in that dir's own
  `LICENSE.md`). ~170 KB total, negligible against the 64 MB web cap.
- **Loading — embedded, not file-path**: new shared module `client/src/
  sfx.rs` (`#[path]`-included by both binaries, same pattern as `world.rs`/
  `ui.rs`) loads all four via `include_bytes!` +
  `RaylibAudio::new_wave_from_memory`/`new_sound_from_wave`, NOT
  `RaylibAudio::new_sound(path)`. Reasoning: the web target has no real
  filesystem — a path-based `LoadSound` would need emscripten's
  `--preload-file` virtual-FS staging, a second asset pipeline that only
  the web binary would use. Embedding the bytes at compile time means one
  code path for both targets and nothing to stage in `build-web.sh`.
- **Device init is a fallible environmental boundary, not a bug condition**:
  `RaylibAudio::init_audio_device()` returns `Result` — treated as
  `Option` (`.ok()`) in both `main()`s, so no sound card / a headless
  environment / a browser autoplay block degrades to silence instead of
  crashing the game. `sfx.rs`'s OWN wav-parsing calls still `.expect()`,
  deliberately: those bytes are our own checked-in, known-good assets, so a
  decode failure there is a build-time asset bug worth crashing loudly on,
  a different kind of failure than "no audio hardware".
- **Web lifetime**: `bin/web.rs`'s `State` is intentionally leaked
  (`Box::into_raw`, never reconstructed/dropped — it lives exactly as long
  as the tab, driven by `emscripten_set_main_loop_arg`). `Sfx<'aud>`
  borrows its `RaylibAudio`, so embedding both in the same struct would be
  self-referential; instead the audio device is `Box::leak`'d to `'static`
  right alongside `state`'s own leak, and `State` stores `Option<sfx::
  Sfx<'static>>` — same tradeoff already made for `state` itself, not a new
  one. Native's `main()` has no such issue (audio device is a plain local
  living for the whole non-leaked loop).
- **Wired to the four existing toast events** (one `.play()` call next to
  each `ui_state.show_*_toast`/`show_info_toast` call, both clients,
  mirrored exactly): `merge.wav` on a new merge-inventory row, `gift.wav`
  on a gift-claim inventory row, `levelup.wav` on an XP-level increase,
  `error.wav` on the eyedropper's "not unlocked" rejection. No new trigger
  points invented — every sound rides an event that already had a visual
  toast, so sound and toast can never drift out of sync.

**Verify status:** `cargo build -p server`, `-p client --bin client`, `-p
client --bin bot` all clean; `./build-web.sh` clean (confirms the
emscripten link succeeds with real audio FFI calls, not just that the Rust
compiles — raylib-sys already builds raudio for emscripten by default, no
build.rs/EMCC_CFLAGS changes needed). `du -sh client/web` = 1.4M, `web.wasm`
1.1M — up from F11's 980K/740K (the embedded wav bytes plus normal growth),
nowhere near the 64 MB cap. Audible playback NOT verified — no speakers/
browser in this environment to hear it, and per this repo's standing
convention the author drives the actual GUI client; REASONED from the code
(lifetime/borrow correctness confirmed by the fact that it compiles under
both targets, including the emscripten self-referential-leak workaround).
NOT committed.

---

**Previous batch:** F12 Polish, part 2/4 — help overlay explaining merge.
`ui::draw_help_overlay` (F9.6 item 5) was keybindings-only — nothing in-game
ever explained the actual theme mechanic ("how do I get new colors?").
Split it into two sections: a new "Merging colors" blurb (touch cursors to
blend hues, long-press a tile as the solo-friendly alternative, merging
earns XP which raises the sat cap) above the existing "Controls" list,
retitled "How to play". Shared via `ui.rs`'s `#[path]` include, so both
clients get it automatically — no `bin/web.rs` changes needed. Fits inside
the existing 600x560 overlay box with room to spare (content ends ~80px
above the bottom edge).

**Verify status:** `cargo build -p client --bin client` and `./build-web.sh`
both clean. Visual layout/wording REASONED only (read the geometry math,
didn't render it) — left for the author's hand-test pass, same convention
as every prior batch. NOT committed.

---

**Previous batch:** F12 Polish, part 1/4 — bots adapted to the new schema +
"Merge with me!" center bot + cursor name labels. plan.md's F12 entry lists
four sub-parts (sounds, bots, help overlay, itch page styling); doing them as
separate batches per the usual protocol rather than one giant batch.

- **Bots — world-cartesian coordinates**: `client/src/bin/bot.rs`'s
  heart/hexagon trajectories were still in the pre-F2 fixed-canvas PIXEL
  space (`CENTER = (360, 360)`, `PATH_RADIUS = 220`) — a leftover from before
  the hex-island multiplayer world existed. `set_pos` has no
  ownership/paint-permission check (bots never paint, only move + name
  themselves), so this was silently harmless to compile (`SetPosArgs` is
  still just `(cx, cy): f32`) but functionally wrong: the bots were looping
  in a tiny, meaningless patch near world-cartesian origin instead of
  orbiting the admin's island. Converted to world units: `CENTER = (0.0,
  0.0)` (the admin's slot-0 center), `PATH_RADIUS = ISLAND_EDGE_REACH + 10.0`
  = 36.0 (`ISLAND_EDGE_REACH` = 26.0, mirroring `world::constants::
  ISLAND_FIT_ZOOM`'s derivation for `ISLAND_RADIUS` = 15 fine hexes) so the
  loop clears the admin island's painted tiles and traces the margin ring
  around it.
- **New "center" bot** (backlog: "bot at the middle with a highlight 'Merge
  with me!'"): third `Shape::Center`, idles in a tight `CENTER_LOOP_RADIUS` =
  5.0 loop right at the world origin (inside the admin's own territory — the
  literal "centre ilot" ask). Its display name IS `"Merge with me!"` — no
  bot-specific rendering needed once cursor name labels exist (next item).
  Own creds key `hexmerge-bot-center`, independent identity like the other two.
- **Cursor name labels** (new, enables the above): `world::draw_cursor_label`
  draws every online other-player's name in a small box above their cursor
  (same visual language as `ui::draw_button_tooltip`), gated on
  `other_cursor_scale >= 0.5` so it doesn't clutter at heavy zoom-out. Wired
  into both `main.rs` and `bin/web.rs`'s `other_cursors` collection (now
  carries `name: String` alongside color/locked). Defensively truncated to 18
  chars client-side — `set_name` has no server-side length cap, and this text
  now comes from another player's row rendered directly into world space
  (previously names only ever appeared in the footer/island-popup, which
  aren't attacker-controlled render surfaces the same way).

**Verify status:** `cargo build -p server`, `-p client --bin client`, `-p
client --bin bot` all clean; `./build-web.sh` clean. `cargo build --workspace`
still fails on the `web` bin under the native target — pre-existing,
expected (`serde_json` is an emscripten-only dependency; `web.rs` only
builds via `./build-web.sh`'s wasm32-unknown-emscripten target, per F5's
standing note). **VERIFIED live** against the local instance: ran `cargo run
-p client --bin bot -- center/heart/hexagon` briefly each, then `spacetime
sql -s local hexmerge "SELECT name, cx, cy FROM user"` — center bot at
`(0.33, 4.99)` (radius ≈5, matches `CENTER_LOOP_RADIUS`), heart-bot at
`(35.5, -12.9)` (radius ≈37.8, consistent with the heart curve's radius at
that phase), hexagon-bot at `(3.2, 31.2)` (radius ≈31.3 ≈ `PATH_RADIUS *
cos(30°)`, exactly the hexagon's inradius at a mid-edge sample point) — all
three land in the expected world-cartesian range, not the old pixel range.
Name-label rendering itself REASONED (read and traced both clients, which
mirror each other) but NOT hand-tested visually, per this repo's standing
convention (author drives the GUI client). NOT committed — pending the
author's hand-test pass, same as every prior batch.

---

**Previous batch:** F11 Flying gift (P2). plan.md's original entry was a
one-liner ("scheduled spawn of a drifting pickup, click/tap to claim ->
random hue or XP") with no author-provided detail, so the executor fleshed
out the design below and folded it back into plan.md's F11 bullet + the
canonical constants table — flag any of these to the author if a different
call is wanted.

- **Schema**: `Gift(id, x, y, spawned_at, expires_at)`, public. At most one
  active gift at a time (simplest P2 scope call, ASSUMED acceptable) — a
  repeating `gift_tick` (every `GIFT_SPAWN_PERIOD_SECS` = 45s) sweeps any row
  past `expires_at` then spawns a fresh one (uniform-in-disk over the
  currently-occupied world bound, reusing `occupied_rings`/`SLOT_SPACING`
  the same way `paint_margin_cell` does) if none remain.
  `GIFT_LIFETIME_SECS` = 25s (< the tick period, so gifts have visible
  down-time rather than one always being up — REASONED, tunable). Lazy-
  seeded in `client_connected`, same pattern as every other schedule table.
- **Drift**: `x`/`y` are the spawn center; the rendered/hit-tested position
  drifts in a small circle around it (`GIFT_DRIFT_RADIUS` = 1.2 world units,
  `GIFT_DRIFT_PERIOD_SECS` = 5s), a pure function of elapsed time since
  `spawned_at` — no continuous position sync needed. Duplicated in exactly
  two places (server's own `gift_drift_pos` using `libm`, `world.rs`'s
  mirror using plain `f32::sin/cos`, shared by both clients via its
  `#[path]` include), matching this codebase's established convention for
  geometry helpers that need per-target trig (see `merge::merge_hue`'s
  comment on why the `shared` crate itself stays libm-free). The three
  numeric constants (`GIFT_DRIFT_RADIUS`/`_PERIOD_SECS`/`GIFT_CLAIM_DIST`) DO
  live in `shared`, same as `ISLAND_RADIUS`/`HUE_TOLERANCE`, since both the
  claim distance check and the render must agree byte-for-byte.
- **Claim**: `claim_gift(gift_id)` — caller must be within `GIFT_CLAIM_DIST`
  (3.0 world units) of the gift's CURRENT drifted position (computed
  server-side from `ctx.timestamp - spawned_at`, not just the static spawn
  point), checked against the caller's last-reported `set_pos` cursor — same
  anti-cheat posture `set_pos`'s own cursor-merge check uses. Reward is a
  coin flip (`rng.gen_bool(0.5)`): a fresh random hue (retried up to 8 rolls
  against one already owned within `HUE_TOLERANCE`, falling back to
  `XP_GIFT` = 20 if all 8 collide) or straight `XP_GIFT`. `Inventory` gained
  a `from_gift: bool` field (appended at the end, same reason
  `Island.border_color` was — `bin/web.rs`'s positional parsing indexes by
  schema order) so a gift-hue's `obtained_with: None` row isn't misread by
  the client's existing inventory-watch as `reset_account`'s reseed.
- **Bug caught during verification, fixed before shipping**: the expired-gift
  guard originally did `ctx.db.gift().id().delete(gift.id); return
  Err(...)`, mirroring nothing else in this file — every other reducer here
  validates fully before any mutation. Live-probed and confirmed: a
  reducer's `Err` return rolls back every write it made in that call, so the
  delete was dead code (the row lingered until the next `gift_tick` sweep
  regardless). Removed the ineffective delete; left the sweep as the sole
  cleanup path, consistent with the rest of the file's guards-before-
  mutation shape.
- **Rendering/input**: world-space pulsing "box + ribbon" icon
  (`world::draw_gift_icon`, built from `draw_poly`/`draw_line_ex` primitives
  already used elsewhere in `world.rs`, not raylib's rectangle calls).
  Click/tap resolves on PRESS (not release) inside the existing
  `suppress_map_until_release` latch — landing on the gift claims it and
  consumes the whole gesture, the same way clicking through a modal's close
  button already does, so the same press can't also start a paint stroke or
  long-press underneath it. Gated on `over_map_area` too (this batch landed
  right after that guard was broadened — see below — so F11's own gesture
  respects it from the start rather than needing a follow-up fix).

**Verify status:** `cargo build -p server`, `-p client --bin client`, and
`./build-web.sh` all clean. Package size unaffected (`du -sh client/web/` =
980K, `web.wasm` 740K — nowhere near the 64 MB cap). **VERIFIED live**
end-to-end via a throwaway Node WebSocket probe (same technique as prior
batches) against the local instance: far-away claim rejected ("too far
away"), an already-expired gift's claim rejected ("gift is gone") with the
row correctly left for the next tick rather than double-deleted, a close
claim after `set_pos`-ing onto the gift's position succeeds and the row
disappears from the table, and BOTH reward branches observed across
repeated runs — a `from_gift: true` inventory row on one run, `user.xp`
incremented by exactly `XP_GIFT` (0 -> 20) on others. Client-side rendering/
click gesture REASONED (read and traced both `main.rs` and `bin/web.rs`,
which mirror each other) but NOT hand-tested by the executor, per this
repo's standing convention — left for the author's own pass. NOT committed
by the executor this batch (the author hand-tests before the next feature
starts, same as every prior batch).

---

**Previous batch:** Out-of-plan bugfix, author-reported: "Clicking on an UI
element shouldn't draw on the map." Root cause was already half-diagnosed in
a standing comment in `main.rs` (the header/footer HUD bands sit ON TOP of
the map, but their screen coordinates still map to SOME world tile via the
camera transform) — that guard (`over_map_area`) had only ever been applied
to the foreign-island hover-reinterpretation check, not to the actual
mouse-driven world-mutation blocks. So pressing/holding LMB over any footer
button (Eraser, Center, My Isle, a last-3 swatch, the name field, ...) fell
straight through into the paint/erase block, since `map_input_allowed` only
ever checked modal-open state, never cursor position. Same gap existed for
the middle-click eyedropper (press position not checked) and long-press-merge
(press over a footer button would still arm a long-press against whatever
tile lay beneath it). Fix: hoisted `over_map_area` to compute once right
after `mouse_screen`/`mouse_world` each frame, and added it as a guard to the
painting block, the middle-click-eyedropper press check, and the long-press
initiation check, in both `main.rs` and `bin/web.rs` (which mirror each other
by convention). REASONED (read and traced the logic in both clients) +
VERIFIED the build: `cargo build -p client --bin client` and `./build-web.sh`
(wasm32-unknown-emscripten) both succeed; `cargo build --workspace --exclude
client` (server) unaffected. Runtime click-vs-paint behavior itself not
independently hand-tested by the executor — left for the author's own
hand-test per this repo's standing convention.

Same-sitting follow-up (author-reported): "You shouldn't render the white
hexagon if you're on HUD" — the world-space hover highlight (`hover_paintable`,
the white hex outline) and the eyedropper "+" hint (`hover_takeable`) had the
identical gap: both classify `mouse_world` regardless of whether the cursor
is actually over the map, so hovering a footer button whose underlying world
tile happened to be paintable/mergeable would flash the white hex or "+"
hint through the header/footer's semi-transparent (alpha 235) background.
Added the same `over_map_area` guard to both, in both clients. REASONED +
VERIFIED build (same two commands as above, rerun clean after this change).

---

**Previous batch:** Author direct edit (own commit, `61adba2 "feat: admin
account"`, concurrent with the executor's F10 batch below) — a real bug fix
found while hand-testing: `main.rs`/`bin/web.rs` each hand-rolled the same
"which modal is open" check in four places, and the web build's copy was
missing two of the four flags, letting a hovered island silently close My
Isle or the Escape/help overlay out from under the player. Consolidated into
`UiState::any_modal_open()` (single source of truth, checked by both
clients) + `close_all_modals()`/`modal_dismiss_clicked()` helpers replacing
the repeated four-flag assignments and close-click checks at every modal's
open/dismiss site. Author hand-tested and confirmed it works. Not
independently re-verified by the executor beyond the author's own report —
logged here for the record, same as previous author-direct-edit batches
(e.g. `cc6a47b "fix: open colors"`).

---

**Previous batch:** F10 Admin (P2, first item — see plan.md's P2 section).
Hexaworld.md's Admin spec: "1 identity with a password that is admin, can
delete tiles, can freeze the game so no one can interact anymore, can
restore backups, holds the center tile."

1. **`claim_admin(password)`** — hashes the input (SHA-256) and compares
   against `constants::ADMIN_PASSWORD_SHA256`; only the digest is committed,
   never the plaintext (repo is public). Grants `config.admin`. Idempotent
   for the current admin. Everyone already gets an island on first connect
   (`client_connected`), so claiming admin relocates the caller's EXISTING
   island to the reserved world-center slot 0 rather than creating a new
   one — swapping slots with whatever island currently holds slot 0 (none
   yet, or a previous admin's if re-claimed under a different identity)
   via the same "bump through a temporary out-of-range slot first" technique
   `rerank_fire` already uses, since `slot` is `#[unique]`.
2. **`set_frozen(frozen)`** — admin-only (`require_admin`); flips
   `config.frozen`. New shared guard `check_not_frozen`, called first by
   every player-facing mutating reducer (`set_pos`, `set_name`, `set_lock`,
   `set_brush`, paint/erase (both island + margin), `merge_with_cell`,
   `reset_account`, like/unlike, `set_island_link`,
   `set_island_border`/`disable_island_border`/`show_island_border`,
   `click_link` — 17 total), rejects with "the game is frozen" while set.
   Scheduled/system reducers (rerank, time-XP, reap, connect/disconnect)
   and the three admin reducers themselves are NOT gated — freeze stops
   PLAYER interaction, not the world's background clocks or the admin's
   own tools (in particular, the admin must always be able to unfreeze).
3. **`delete_island_cells(island_id)`** — admin-only; wipes every
   `island_cell` row for the target island (moderation for offensive art).
   The island row itself (ownership, likes, link, border, slot) is
   untouched.
4. **Ops docs** (`WORK.md`, new "Admin (F10)" section) — `spacetime call`
   examples for all three reducers against `-s local`/prod, how to rotate
   the password (regenerate the SHA-256 digest, swap the constant), and a
   "Backups" subsection: plain `spacetime sql` dump-per-table commands
   (Hexaworld.md's "can restore backups" — manual restore-by-hand, no
   automated snapshot/replay, matching plan.md's "admin tooling is minimal
   for now" scope).

**Verify status:** `cargo check -p server`/`-p client --bin client` and
`./build-web.sh` all clean (both the wasm32-unknown-unknown module target
and the emscripten web target — `sha2` compiles fine on both). **VERIFIED
live** end-to-end via a throwaway Node WebSocket probe (same v1.json.
spacetimedb technique as prior batches) against the local instance, with
`spacetime sql` polling for ground truth between steps (the probe's own
CallReducer/TransactionUpdate ack-matching timed out for an unclear reason,
but every DB-side effect below landed exactly as expected, so the sql
checks are what's cited as VERIFIED):
   - Wrong password: `config.admin` unchanged.
   - Correct password: `config.admin` set to the caller; caller's island
     slot moved from its original value to `0`. Re-run with the same
     (already-admin) caller: no-op, slot stays `0` (idempotence).
     Re-run as a DIFFERENT identity with the correct password (a second
     probe run): the NEW caller's island took slot 0, and the PREVIOUS
     admin's island was swapped into the new caller's vacated slot instead
     of being deleted or colliding — exercises the trickier swap-with-
     existing-occupant path, not just the empty-slot-0 case.
   - Non-admin `set_frozen(true)`: `config.frozen` unchanged.
   - Admin `set_frozen(true)`: `config.frozen` flips to `true`.
   - `paint_island_cell` while frozen: rejected, `island_cell` row count
     unchanged.
   - Admin `set_frozen(false)` then `paint_island_cell`: succeeds, row
     count +1 — confirms `check_not_frozen` isn't a one-way latch.
   - Non-admin `delete_island_cells`: rejected, `island_cell` row count
     unchanged.
   - Admin `delete_island_cells(<island_id>)`: every `island_cell` row for
     that specific island gone, confirmed via a filtered `spacetime sql`.
   All 17 `check_not_frozen` call sites are the identical one-line guard;
   only `paint_island_cell` was exercised live end-to-end, the other 16 are
   REASONED by code identity with that verified call site (same pattern
   this file has used for sibling reducers in prior batches). No admin UI
   in either client — F10 is CLI-only ops tooling per plan.md's "admin
   tooling is minimal for now," so nothing to hand-test in the GUI. Local
   instance re-published clean (fresh state, no leftover test identities)
   after the probe run.

Files touched: `server/Cargo.toml` (+`sha2`), `server/src/lib.rs`
(`ADMIN_PASSWORD_SHA256` constant, `hash_password`/`require_admin`/
`check_not_frozen` helpers, `claim_admin`/`set_frozen`/`delete_island_cells`
reducers, `check_not_frozen(ctx)?;` added to 17 existing reducers),
`WORK.md` (Admin + Backups sections).

**A note on the password:** since this repo is public, I (the executor)
generated a random password and committed only its SHA-256 digest — I did
not choose one myself for the author to guess or reuse, and the plaintext is
deliberately NOT recorded anywhere in this repo. It was reported once,
directly to the author in chat, for private storage. Rotate it any time via
the command in `WORK.md`'s new Admin section if a memorable one is preferred
before it's ever used against production.

---

**Previous batch:** Author follow-up on the border-customization feature
(same day, 2026-07-11) — "cleaner way to change Isle border": a single
"Border: Shown/Hidden" toggle instead of the separate status line + "Disable
border" button, and "Set border to current color" should preview the
pending change as `[current border color] -> [current cursor color]`
instead of a bare label.

1. **New reducer `show_island_border`** — the old toggle idea (re-showing =
   just call `set_island_border` again) would have silently repinned
   `border_color` to the current brush every time a player un-hid their
   border, which isn't what "Shown/Hidden" implies. Added a reducer that
   only flips `border_hidden` back to `false`, leaving `border_color`
   untouched — mirrors `disable_island_border`'s "find caller's own island,
   update one field" shape.
2. **Client wiring, both targets** — `ui.rs`: `IslandInfo.border_color: Color`
   (the resolved display color — custom pin else seed-hue default — same
   fallback the map-render code already uses); `Actions::show_island_border`;
   `border_disable_btn_rect` renamed `border_toggle_btn_rect`, now fires
   `show_island_border` or `disable_island_border` depending on
   `popup.border_hidden`, with its own label carrying the state ("Border:
   Shown"/"Border: Hidden", red-tinted while hidden) — the separate status
   line is gone. `border_set_btn_rect`'s button now draws two 16px swatches
   with a `->` between them (border color, then the live brush color via
   `world::hsv_color(info.brush...)`), so `draw_island_popup` gained an
   `&HudInfo` parameter it didn't need before. `main.rs`/`bin/web.rs`: new
   `resolve_border_color` helper (mirrored in both) computes the swatch color
   at `open_island_info`/`refresh_island_popup` time from `island.border_color`
   or a same-owner inventory scan for the seed hue; both reducer-dispatch
   blocks gained the `show_island_border` arm.

**Verify status:** `cargo build -p server`, `-p client --bin client`, and
`./build-web.sh` all clean (web package still ~1MB total, nowhere near the
64MB cap). `spacetime call hexmerge show_island_border -s local` against the
local instance returned clean (exit 0, no reducer error) for an identity that
genuinely owns an island — VERIFIED the reducer runs and the ownership check
passes; did not re-verify the packed-color-untouched behavior with a fresh
`set_island_border` → `disable_island_border` → `show_island_border` →
inspect-row sequence this round (REASONED only, by code trace — the
underlying update pattern is identical to the already-VERIFIED
`disable_island_border`/`set_island_border` pair from the batch below, just
with `border_hidden: false` in place of `true` and no `border_color` touch).
The swatch-preview UI itself is REASONED, not yet author-hand-tested through
the actual GUI, per the standing protocol. NOT published to the production
VPS.

**Previous batch:** Backlog item, author override (2026-07-11) — "Customise
your Isle border color or make it transparent (remove)". `other_ideas.md`
and plan.md's own backlog entry had deferred this as "not worth a schema
cycle before the freeze" (submission is 2026-07-12 18:00 UTC); asked the
author to confirm given that note, they chose to override it and land the
feature now.

1. **Schema + reducers** — `Island` gets `border_color: Option<u32>` (packed
   HSV) and `border_hidden: bool`, both appended AFTER the existing fields
   (not inserted among them) so `bin/web.rs`'s hand-rolled positional row
   parser — which reads fields by schema-order index, not by name — doesn't
   shift under it. Two reducers: `set_island_border` (packs the caller's
   CURRENT `user.hue/sat/val` into `border_color`, also clears
   `border_hidden`) and `disable_island_border` (sets `border_hidden`,
   leaves `border_color` alone). Both follow `set_island_link`'s existing
   "find caller's own island via `.owner().find(ctx.sender())`" pattern.
2. **Client wiring, both targets** — `ui.rs`: `IslandInfo.border_hidden` +
   `Actions::{set_island_border, disable_island_border}` + two new buttons
   ("Set border to current color" / "Disable border") and a status line in
   the own-island popup, below the link editor. `main.rs` and `bin/web.rs`
   (kept in parity, as always): `open_island_info`/`refresh_island_popup`
   thread `border_hidden` through; the per-island border-color computation
   now checks `border_hidden` (nothing drawn) then `border_color` (custom)
   before falling back to the pre-existing seed-hue default.

**Verify status:** `cargo build -p server`, `-p client --bin client`, and
`./build-web.sh` all clean. Published to the local instance and exercised
live via `spacetime call`/`spacetime sql` (not the GUI client, per the
standing "author drives real runtime testing" protocol): `set_island_border`
packed `(hue=57, sat=40, val=100)` into `border_color` correctly for a
fresh test identity; `disable_island_border` flipped `border_hidden` to
`true` while leaving `border_color` untouched; re-running
`set_island_border` un-hid it (`border_hidden` back to `false`) — all
VERIFIED. Client-side rendering precedence (REASONED, not yet author-
hand-tested): traced the new branch in both `main.rs` and `bin/web.rs`,
matches the reducer semantics above. NOT published to the production VPS
(`spacetime.mister-esman.uk`) — that's a separate, explicit step left for
the author.

**Previous batch:** Second author follow-up in the same sitting — the
pencil icon is STILL reported invisible after the first widen-and-outline
fix, plus one more tooltip content item.

1. **Center button tooltip** — added to `BUTTON_TOOLTIPS`: "Center" / "Go
   back to your island".
2. **Pencil icon still invisible, round 2** — re-verified the geometry by
   hand-tracing the actual rotated coordinates for the current parameters
   (center (261,698), half_w 4.5, angle 45°): all 7 vertices land well
   inside the 30x30 button bounds, form non-degenerate quads/triangle with
   several-px extents, using `d.draw_triangle`/`draw_quad` (a thin wrapper
   around the same call) — the exact primitive `draw_eraser_icon`,
   `draw_cursor_scaled`, and `draw_heart` already use successfully in this
   same file (confirmed working, per the author's own "the eraser is
   great"). Checked raylib's actual `DrawTriangle` C source
   (`raylib/src/rshapes.c`) to rule out a winding/culling explanation —
   it's a textured-quad immediate-mode fill with no backface culling in
   this render mode, consistent with this file already using both
   triangle windings successfully elsewhere. Found no code defect. Widened
   further as insurance (`half_w` 4.5 -> 6.0, outline 1.5px -> 2.0px) in
   case of a rendering subtlety (window scaling / anti-aliasing at small
   sizes) not visible from static analysis, but flagging this in status.md
   rather than claiming confidence I don't have: the leading real-world
   explanation is a stale build — this is a native `cargo run` process the
   author manages themselves (doesn't hot-reload) or a cached web bundle,
   not something I can rule out from here per the standing "author drives
   real runtime testing" protocol. Needs the author's confirmation of a
   fresh rebuild+restart before treating this as still-open.

**Verify status:** `cargo check -p client --bin client --bin bot` and
`./build-web.sh` both clean (the latter now also compiles the new `shared`
crate pulled in by a concurrent author refactor — see `client/src/world.rs`/
`server/src/lib.rs`, unrelated to this batch). Item 2 unresolved pending
the author's rebuild confirmation.

**Round 3 (same sitting):** author confirmed testing the web build served
locally, hard-refreshed/reloaded past cache, and the pencil is STILL not
visible — ruling out staleness, this is a real rendering bug. Rather than
keep tuning size/color on the same construction, rebuilt `draw_pencil_icon`
from scratch on axis-aligned primitives only: `draw_rectangle_rounded` +
`draw_rectangle_rec` + `draw_triangle` + `draw_rectangle_lines`/
`draw_triangle_lines`/`draw_rectangle_rounded_lines` — the exact same calls
`draw_eraser_icon` (proven working) already uses, just recolored/resized
into a pencil silhouette (pink cap, white body, tan tip). Removed the now-
dead `rotate_around`/`draw_quad` helpers entirely (no longer used by
anything) rather than leave them as unreferenced code. The working theory:
the two failed attempts both went through a hand-rolled rotated-quad-via-
two-triangles construction that isn't used ANYWHERE else in this codebase
— possible it interacts badly with something specific to the emscripten/
WebGL path (untested combination), whereas every other icon in this file
(including the still-diagonal eraser cap triangle) uses plain, unrotated
vertices. Trading the diagonal "pencil" look the author picked from the
Artifact mockup for a horizontal one to eliminate that whole code path as
a variable, given two rounds already spent on the diagonal version. `cargo
check -p client` and `./build-web.sh` both clean, no warnings (confirms
the removed helpers were truly dead, not silently still referenced).
Genuinely unverified beyond that — needs the author's next test to confirm
this one actually renders.

---

**Previous batch:** Author follow-up on the batch immediately below — a
pencil-visibility bugfix the author caught live, plus two more
`other_ideas.md` items taken in the same sitting.

1. **Pencil icon invisible (author-caught, real bug)** — "The eraser is
   great but I can't see the pencil icon." Root cause: the first cut
   (`half_w` 3.0, no outline, tip color `(70,55,45)`) computed correct,
   non-degenerate geometry — verified by hand-tracing the rotated
   coordinates — but a 6px-wide diagonal sliver with a tip color barely
   lighter than the button's own `(40,40,48)` background reads as a faint
   smear at 30px, not a recognizable shape. Fixed in `draw_pencil_icon`:
   widened the body (`half_w` 3.0 -> 4.5), brightened the tip to a tan
   `(190,150,100)`, and added a dark `(40,40,48)` stroke around the full
   pentagon silhouette (same role `draw_eraser_icon`'s stroke already
   plays) so it reads as a crisp shape against the dark button regardless
   of fill color. `cargo check -p client` clean.
2. **Footer button hover tooltips** — new `hovered_button_tooltip`/
   `draw_button_tooltip` in `ui.rs`: hovering Colors, Eraser, Lock, Account,
   or My Isle shows a title + 1-2 line description in a small box glued
   near the cursor (flips above/below the cursor to stay on-screen — footer
   buttons sit near the bottom edge, so the default is ABOVE the mouse,
   opposite of the foreign-island tooltip's default-below), same visual
   language as `draw_island_tooltip`. Content is the author's own copy
   verbatim. No hover delay (unlike the 200ms island-info hover) — these
   are small stationary buttons, not paint-stroke sweep zones, so an
   instant tooltip doesn't risk flickering during normal play. Wired as the
   lowest-priority branch in `draw()`'s modal-exclusivity else-if chain, so
   it never fights the overlay/account/popup/help-overlay modals for the
   same screen space.
3. **ws-status position (web only)** — `bin/web.rs` drew `"ws: {status}"`
   at bottom-LEFT (10, 700) and the FPS counter at bottom-right (640, 700)
   — two debug readouts in opposite corners. Moved the ws-status label to
   (640, 682), directly above the FPS box, per the author's ask.

**Verify status:** all three targets (`cargo check -p server`, `cargo check
-p client --bin client --bin bot`, `./build-web.sh` release/emscripten)
clean, no warnings. Item 1's fix is REASONED from the corrected geometry/
color math, not yet re-confirmed visually by the author (the very thing
that caught the original bug) — flagging for a second look. Items 2-3 are
new UI code, not hand-tested in a running client by me, per this repo's
standing protocol.

Files touched this batch: `client/src/ui.rs` (`draw_pencil_icon` rewrite,
`hovered_button_tooltip`, `draw_button_tooltip`, `BUTTON_TOOLTIPS`,
`draw()` wiring), `client/src/bin/web.rs` (ws-status position).

---

**Previous batch:** Author-requested UX polish, five `other_ideas.md` items
taken directly (not routed through plan.md batching — a short, well-scoped
punch list the author asked for in one sitting):

1. **Launch intro easing** — swapped the ease-out-cubic
   (`1.0 - (1.0 - t).powi(3)`) for a new shared `world::ease_in_out_circ`
   (standard two-quarter-circle formula) in both `main.rs`/`bin/web.rs`'s
   intro camera ease. Matches the author's explicit ask (slow start, fast
   middle, gentle landing) — `other_ideas.md` had this flagged as
   deliberately NOT done from an earlier batch's ease-out choice.
2. **Eraser icon** — the author didn't like the existing two-tone eraser
   glyph and asked to choose a replacement themselves. Published an
   Artifact with 6 candidate icons (all drawable with the same raylib
   primitives the real icon uses — rectangles, triangles, lines, circles —
   no image assets, consistent with `ui.rs`'s "primitives only" rule),
   rendered at the real in-game colors/button size. Author picked pencil
   (option E's tip) with a twist beyond the original prompt: the paint/erase
   toggle button now shows a diagonal pencil (`draw_pencil_icon`, new) by
   default — the icon reflects which TOOL is currently active, not a
   static "click to erase" glyph — and swaps to the original eraser icon
   plus a red outline (`draw_rectangle_lines_ex`, replacing the old solid-
   red-fill active state) while erasing is on. `client/src/ui.rs`, shared
   by both clients.
3. **Locked cursor border** — `draw_cursor`/`draw_cursor_scaled`
   (`world.rs`) take a new `locked: bool`, drawing a 3px black outline
   (vs 1px normally) via `draw_line_ex` per edge instead of
   `draw_triangle_lines` (no thickness parameter). Wired for both the
   caller's own cursor AND every other online player's cursor — `user.locked`
   is already in the subscription, so this was a small, essentially free
   extension beyond just the caller's own cursor, and lets you tell at a
   glance whether someone else has Lock on (no merge will trigger) without
   opening their info popup.
4. **Leaderboard (rerank) tile-count metric** — `rerank_fire` (server)
   sorted by likes desc then `created_at` only; now sorts by likes desc,
   then total painted-cell count desc (one O(n) pass over `island_cell`
   building a per-island `HashMap` count, since this only runs once per
   `RERANK_PERIOD_SECS` tick, not per-frame), `created_at` kept as the
   final tiebreak. Matches Hexaworld.md's "hidden leaderboard" concept —
   now rewards active painters too, not just liked islands.
5. **Hover-tile border visibility at low zoom** — the hover highlight's
   outline (`draw_poly_lines_ex`) used a fixed WORLD-unit thickness (0.06),
   the exact same failure mode the island border had before its own
   zoom-independent fix earlier in this file: shrinks under a screen pixel
   and vanishes zoomed out. Applied the identical fix — new
   `world::constants::HOVER_BORDER_PX` (2.0) divided by `camera.zoom` at
   the draw site, in both clients — so it now "acts like the ilot border",
   per the author's own phrasing.

**Verify status:** `cargo check -p server`, `cargo check -p client --bin
client --bin bot`, and `./build-web.sh` (release, emscripten target) are
all clean, no warnings — REASONED/build-verified only. Item 4 (server
change) republished to the local instance (`./server/publish.sh`) and
bindings regenerated (`./generate_module_bindings.sh`); no schema change,
so `module_bindings/` came out byte-identical (`git status` confirms
nothing to commit there) — republish itself succeeded, but the actual
re-sort behavior wasn't exercised live (needs several islands with
differing like/tile counts and a real rerank tick, left for the author's
hand-test). Items 1/2/3/5 are client rendering only, not hand-tested in a
running client by me, per this repo's standing protocol (the author drives
real runtime testing); the eraser-icon choice itself came from the author
reviewing the rendered Artifact preview directly rather than a code read,
which is as close to a visual sign-off as this protocol gets pre-hand-test.

Files touched: `client/src/world.rs` (`ease_in_out_circ`, `draw_cursor`/
`draw_cursor_scaled` locked param, `HOVER_BORDER_PX`), `client/src/ui.rs`
(`draw_pencil_icon`, `rotate_around`, `draw_quad`, footer eraser-button
draw), `client/src/main.rs`, `client/src/bin/web.rs` (intro ease, cursor
call sites, hover border thickness), `server/src/lib.rs` (`rerank_fire`
tile-count sort).

---

**Previous batch:** Author hand-test follow-up on F9.6 (own commit,
`cc6a47b "fix: open colors"` — the author ran the client, found issues, and
patched them directly rather than routing through the executor; logged here
for the record, same as previous author-direct-edit batches).

- **Real bug, VERIFIED by the author's own hand-test:** clicking the
  "Colors" footer button opened the inventory overlay and then immediately
  closed it again on the very same click. Cause: F9.6 item 4's new
  "click outside `overlay_rect()` closes it" check ran unconditionally
  after the footer-button handling above it — the footer sits below
  `overlay_rect()` (y ~676-720 vs the panel's y 60-620), so the SAME click
  that had just opened the overlay also matched "click landed outside the
  panel" and closed it one line later, in the same `handle_input` call.
  Fixed with an early `return actions;` right after the inventory-overlay
  toggle, so opening it consumes the click instead of falling through to
  the outside-click check. This was a real regression introduced by item 4
  that a live test caught immediately — worth remembering for any future
  "close on outside click" additions elsewhere in this file (the
  account-overlay/help-overlay toggles already `return` for the same
  reason, now consistently).
- **Footer layout reverted, author-requested:** `FOOTER_H` back to 44 (the
  single-row footer from before F9.6) now that Eraser and Lock are
  icon-only buttons (`draw_eraser_icon`/`draw_lock_icon` — a two-tone
  eraser glyph and a padlock with an open/closed shackle) instead of
  text-label buttons, so they fit to the left of the name field on the
  original single row. My item-1 change that grew the footer to 78px for a
  second row is superseded by this.
- **Constants re-tuned by hand-testing feel**, same live-tuning pattern as
  `MERGE_DIST`/`MARGIN_GAP_TILES` earlier: `INTRO_DURATION` 1750ms -> 3000ms
  (launch intro eases slower), `BORDERLESS_ZOOM_THRESHOLD` 5.0 -> 2.0 (tile
  outlines now only drop out much further into far-zoom than the executor's
  original guess).

Not independently re-verified by me beyond a clean `cargo check` — this was
the author's own hand-test-and-fix cycle on their machine, committed
directly.

---

**Previous batch:** Out-of-plan mobile bugfix, then all of F9.6 (items 1-8,
one combined batch at the author's request — normally 1-2 items/batch, but
this was a coherent UX polish sweep and the author asked for the whole thing).

**Mobile bugfix (author-reported mid-batch, before F9.6 started):**
double-click-to-like wasn't registering on a real phone. Root cause
(REASONED, matches the symptom exactly): `game.html`'s canvas is fixed at
720x720 internal render resolution but only `width: 100vmin` on screen —
on most phones that's well under 720 CSS px, so the browser upscales, and
any physical finger-jitter between the two taps of a double-tap gets
magnified by that same ratio once mapped into game-space pixels.
`LONG_PRESS_TOL_PX` (8px, tuned for mouse precision) was used both for the
single-press hold-still/drag check AND for matching the second tap's
position against the first — the fix adds a separate, more forgiving
`DOUBLE_CLICK_TOL_PX` (28px) used ONLY for the second-tap-position-match,
leaving the existing single-press tolerance untouched (narrowest fix that
addresses the reported symptom without loosening drag-vs-click detection
elsewhere). Mirrored identically in `main.rs` and `bin/web.rs`. Both clients
build clean. REASONED, not independently re-verified on real touch hardware
by me (no phone available) — awaiting the author's next mobile hand-test.
touch-action:none was already set on the canvas, ruled out as a cause.

**F9.6 items 1-8:**
1. **Eraser** (decision 18) — `erase_island_cell`/`erase_margin_cell`
   reducers added, same validation + token charge as the matching paint
   reducer (`erase_island_cell`: radius bound + caller-owns-island, scoped
   entirely via `ctx.sender()` same as `paint_island_cell` — structurally
   can't target another player's island, no separate cross-identity check
   needed; `erase_margin_cell`: territory + canvas-bound check only, no
   ownership, matching `paint_margin_cell`'s "everyone's to paint" design).
   **VERIFIED live** via a throwaway Node WebSocket probe (same technique as
   the F5/F9.5-item-10 probes): fresh identity, paint then erase an island
   cell AND a margin cell, confirmed via `spacetime sql` that both rows were
   actually gone afterward (not just a "committed" status); also confirmed
   `erase_margin_cell` rejects the exact same coordinate `paint_margin_cell`
   rejects ("cell belongs to an island") — validation parity, not just
   independently-plausible logic. Republished locally, bindings regenerated
   (`erase_island_cell_reducer.rs`/`erase_margin_cell_reducer.rs`).
   Client: `X` key (not `E` — item 6 claims that for zoom) + new footer
   toggle button toggle paint/erase mode (`UiState.eraser_on`, shared
   `ui.rs`); painting block in both `main.rs`/`bin/web.rs` branches to the
   erase reducer when on; own cursor renders gray + a small eraser badge
   instead of the brush-hue pointer. `FOOTER_H` grew 44->78 (a second row)
   to fit the new button without cramming the already ~6px-of-slack first
   row. Client-side wiring REASONED (compiles clean, both targets) —
   awaiting author hand-test for feel/layout.
2. **Middle-click eyedropper** — clean middle press+release within 8px
   (`MIDDLE_CLICK_TOL_PX`, doesn't disable the existing middle-drag pan,
   which runs off `is_mouse_button_down` every frame regardless and treats
   a real click's near-zero delta as a no-op pan) picks the hovered tile's
   exact h/s/v (any island's cell or a margin cell — `painted_color_at`,
   new in both clients); applies via `set_brush` if the hue is already
   owned (client-side `have_hue` check, server re-validates), saturation
   clamped to the level's `sat_cap`; otherwise a new plain-text toast
   ("not unlocked — long-press to merge") via `UiState::show_info_toast`
   (required making `Toast.hue` an `Option` so the flash swatch is
   optional). Mouse only, per plan.md — no touch path added. REASONED,
   builds clean both targets.
3. **Heart icon for like** — `world::draw_heart` (two circles + a triangle,
   filled = liked, outline = not), replacing the "Likes: N" text-only line
   in both the own-island popup and the foreign-island hover tooltip, both
   clients (shared `ui.rs`). REASONED — no screenshot capability here to
   confirm the pixel composition actually reads as a heart at 14-18px;
   flagged for the author's visual check.
4. **Color overlay ergonomics** — Escape closes it (folded into item 5's
   Escape handler below); clicking anywhere outside `overlay_rect()` also
   closes it now (previously only the X button did); swatches sorted by hue
   (`sorted_hues`, used identically by the click hit-test and the draw loop
   so indices stay in sync); hovering a swatch shows its hex code (of the
   swatch AS RENDERED, i.e. at the current brush sat/val, not a canonical
   100/100) in a small label above it. REASONED, builds clean.
5. **Keybindings/help overlay** — Escape with nothing open shows a minimal
   controls list (`UiState.help_open`, `draw_help_overlay`); Escape with
   any one of {help, inventory overlay, account overlay, island popup} open
   closes THAT one (checked in that priority order, one per keypress).
   Every place that opens one of the other three overlays now also clears
   `help_open`, keeping the "exactly one modal at a time" invariant item 4
   already relied on. REASONED, builds clean.
6. **Keyboard + right-drag camera** — WASD/arrow keys pan (speed divided by
   zoom so it feels like a constant SCREEN speed, same trick already used
   for border thickness), Q/E zoom (wheel untouched); right-click-drag pans
   (added to the existing middle-drag/Shift+left-drag `panning` condition);
   all swallowed while `UiState::text_field_focused()` (new: name/import/
   link-edit) is true, so typing doesn't drive the camera. Mirrored
   identically in `main.rs`/`bin/web.rs`. REASONED — the web build's
   keyboard path additionally assumes the canvas/document receives key
   events without needing explicit focus (untested on a real page load by
   me); mouse/touch paths are unaffected either way.
7. **Launch intro** — first frame the player's own island resolves, the
   camera starts at a "whole occupied world" framing (`world_fit`: bounding
   box of every known island's center, zoomed to fit) and eases
   (cubic ease-out) to the player's island over 1.75s; any mouse button,
   wheel, keypress, or (web only, since touch matters most there) an active
   touch point skips straight to the final pose. Replaces the old instant
   snap; `centered_on_island`'s meaning is unchanged ("camera has settled,
   other logic can take over"), only how it gets there. REASONED, builds
   clean both targets — timing/ease feel not hand-tested by me.
8. **Borderless far zoom** — `world::draw_hex`'s outline param is now
   `Option<Color>`; both clients compute `show_tile_outline = camera.zoom
   >= BORDERLESS_ZOOM_THRESHOLD` (5.0, executor's pick within the author's
   ~4-6px note) once per frame and pass `None` past that zoom to skip the
   per-tile outline draw call entirely (both the island-cell and
   margin-cell draw sites). REASONED, builds clean.

**Verify status:** server-side eraser reducers VERIFIED live (see item 1).
Everything else in this batch is client rendering/input — REASONED from
code + both `cargo check`/`cargo build --release` (native) and the
emscripten `wasm32-unknown-emscripten` target (native `cargo check` clean,
`./build-web.sh` release build succeeds, `du -sh client/web` = 952K, well
under 64 MB) are clean with no warnings, but none of it has been hand-tested
in an actual running client by me — per this repo's standing protocol, the
author drives real runtime testing (`cargo run -p client --bin client`,
and the web build in a browser/phone) rather than me launching the GUI.
Republished server picks up both the eraser reducers and the author's
concurrent live constant tuning (`MARGIN_GAP_TILES`=6, `MERGE_DIST`=1.5) —
no conflict, both landed in the same publish.

Files touched: `server/src/lib.rs` (2 new reducers), `client/src/main.rs`,
`client/src/bin/web.rs`, `client/src/ui.rs`, `client/src/world.rs`,
`client/src/module_bindings/{erase_island_cell,erase_margin_cell}_reducer.rs`
+ regenerated `mod.rs`.

---

**Previous batch:** F9.5 item 10 — dead-player reap (last item in the
sweep). Scheduled reducer (`reap_dead_players`, new `reap_schedule` table,
60s repeating tick, same lazy-seeding pattern as `time_xp_schedule`/
`rerank_warn_schedule`) deletes a player entirely once they're offline,
stale 5+ minutes (`REAP_IDLE_SECS`), their island has zero painted cells,
and their inventory has at most 1 row (the seed hue — XP is deliberately
excluded from the test, since idle time-XP ticks may have granted a few by
the time someone's stale enough to qualify). Deletes, in order: the
island's `island_like` and `island_link_click` rows, the user's
`inventory` rows, the `island` row, then the `user` row. Accepted edge case
(matches plan.md's own call): a reset veteran idling 5 minutes with a still-
empty island gets reaped too — indistinguishable from "never played" with
the data available, and acceptable for the jam.

**Two correctness fixes this required, both real bugs waiting to happen the
moment ANY island could ever be deleted** (neither reachable before this
batch, since nothing ever deleted an island until now):
1. **Slot assignment used `ctx.db.island().count() + 1`.** Once reaping can
   delete an island out of the middle of the sequence, `count()` under-
   reports the highest slot actually in use — the next new player could get
   handed a slot a still-live island already owns, tripping `Island.slot`'s
   `#[unique]` constraint. Replaced with `lowest_free_slot` (new helper): a
   sorted scan for the smallest unused slot `>= 1`, which both avoids the
   collision and reuses a reaped player's freed slot instead of letting the
   world grow unbounded — actually "frees the slot" the way plan.md's
   wording asks for, not just frees a user row.
2. **`paint_margin_cell`'s canvas-bound check used the same `count()` as a
   proxy for "highest slot in use."** Same under-reporting problem — after a
   reap, the margin bound could shrink below cells that legitimately border
   a still-live high-numbered island, wrongly rejecting valid paints there.
   Changed to the actual `max(slot)` over live islands.

**VERIFIED live**, not just by inspection — republished locally with
`REAP_PERIOD_SECS`/`REAP_IDLE_SECS` temporarily dropped to 5s each (restored
to 60s/300s before this final publish), opened a real WebSocket connection
(Python, same technique as item 1's probe) which created a genuine user +
island, closed it cleanly, waited past the shortened idle window, and
confirmed via `spacetime sql` that BOTH the `user` and `island` rows were
gone afterward — while three OTHER, still-`online: true` sessions (the
author's own live hand-testing browser tabs, reconnecting after this
batch's earlier republishes) were correctly left untouched, proving the
`!user.online` guard holds under real concurrent load, not just a
single-player synthetic test. Also incidentally confirmed `lowest_free_slot`
working correctly in the same live run: the remaining 3 real islands held
slots 1/2/3 with no gaps despite earlier reaps having freed slots out of
the middle of the sequence. Republished with the restored real constants,
regenerated bindings (new `reap_schedule_type.rs`), both clients build
clean (no client-side changes needed — this is entirely server-observed
through rows disappearing from existing subscriptions).

---

**Previous batch:** F9.5 — one more author follow-up, immediately after the
zero-gap tiling landed: "great, i just want a gap of 2 or 3 now" (not
literally zero after all — a little breathing room).

With the hex-of-hexes basis now in place, introducing a uniform gap turned
out to be a small, clean change rather than another geometry rewrite:
placed islands as if they were ONE TILE BIGGER than they really are (a new
`SLOT_PLACEMENT_RADIUS = ISLAND_RADIUS + 1` feeds `SLOT_U`/`SLOT_V` instead
of `ISLAND_RADIUS` directly), while every actual paintable/territory check
elsewhere still uses the real `ISLAND_RADIUS` (13) unchanged. Since the
placement radius and the real radius are now decoupled, the concentric
"buffer ring" left over between a real island's edge and its bumped
placement cell's edge becomes genuine margin space — 1 tile from EACH of two
neighbors, 2 tiles total between any two real islands, uniformly (not just
at the 6 axis corners — this is a property of the same tiling identity,
just parameterized by a bigger virtual radius; verified with the exact same
kind of computational check as the zero-gap case, not assumed by symmetry
alone).

Picked `+1` (giving a 2-tile gap) over other bump amounts because the
achievable gaps from this family only come in odd hexdist steps
(equivalently, even tile-margin steps: bump `+1` -> 2-tile margin, `+2` ->
4-tile margin, and so on) — 2 tiles lands squarely in the author's stated
"2 or 3" range, so no half-integer workaround was needed. `SLOT_SPACING`
(still just "hexdist between adjacent island centers", the only externally
meaningful number) updated to match: `2 * (ISLAND_RADIUS + 1) + 1` = 29;
re-verified the `paint_margin_cell` ring-reach identity still holds
(`SLOT_SPACING` per ring, checked rings 1-11 again with the bumped radius —
still exact, since that identity depends only on the placement basis, not
on `ISLAND_RADIUS` itself).

**VERIFIED**: both `cargo build -p server` and the client (native + web)
clean, republished + regenerated bindings (no schema change). Screenshotted
6 real islands again after the change: a clean, uniform dark gap now visible
between every pair of neighbors — narrower than the original ~3-tile margin
band, wider than touching, matching the ask.

---

**Previous batch:** F9.5 — supersedes the immediately-previous batch. Author
follow-up, twice: first "I just dont want any triangular gaps, not just
smaller" (ruling out the SLOT_SPACING-only tweak below as insufficient),
then the actual diagnosis — "change the disposition so that sides of the
hexagons are facing each other". That's exactly right: the previous batch
only shrank the same-axis coarse spacing, but a naive same-axis scheme
places neighboring islands along the axial grid's VERTEX directions, not
its flat-edge directions — no spacing value on that scheme can ever reach
zero gap, it can only shrink the (always-triangular) gap down to nothing
while never actually closing it. Confirmed this numerically before writing
any Rust: swept `SLOT_SPACING` 22..29 under the current formula and the
uncovered-cell count never reaches zero (minimum 576 uncovered cells out of
3721 sampled, even at spacing=26 where islands' hex regions already
mathematically touch).

**Real fix**: islands need to sit on a genuinely different coarse lattice —
the standard "hex-of-hexes" tiling used for packing hex-distance-R
"super-hexagons" (`3R²+3R+1` cells each; 547 for `R=13`) with zero gap AND
zero overlap. Its two generators are `SLOT_U = (R, R+1)` and `SLOT_V =
(-(R+1), 2R+1)` — verified by exhaustive search over small integer bases
(not guessed): computed, for many candidate generator pairs, whether every
fine cell near the origin is covered by AT MOST one island (no overlap) as
well as at least one (no gap); this pair is the one that satisfies both
(others found during the search covered every cell but with real overlaps —
a subtly different, and wrong, kind of "no gap").

The pleasant surprise: `slot_coords`'s ring-spiral algorithm (which islands
get which abstract integer index, spiraling outward in rings of `6*ring`)
didn't need to change AT ALL — it was always just enumerating positions in
an abstract coordinate space using the standard 6 axial unit directions,
never assuming anything about how that space maps to fine-grid pixels. Only
the two functions that DO that mapping needed to change:
- `slot_center(q, r)`: was `(SLOT_SPACING*q, SLOT_SPACING*r)` (independent
  per-axis scaling); now `q*SLOT_U + r*SLOT_V` (a proper 2D linear
  combination of the new basis).
- `in_any_island_territory(q, r)`: needs the INVERSE map (fine cell -> which
  coarse slot owns it) to find the nearest slot candidate before the
  existing "check candidate + its 6 neighbors" defensive scan (unchanged) —
  was a simple per-axis divide (`cube_round(q/SLOT_SPACING, r/SLOT_SPACING)`,
  valid only because the old basis was axis-aligned); now a real 2x2 matrix
  inverse (`SLOT_DET` is the basis matrix's determinant, which not
  coincidentally equals 547 — the per-island cell count — which is the
  actual algebraic reason this tiles perfectly rather than a happy
  coincidence).

`SLOT_SPACING` itself stays defined (`2*ISLAND_RADIUS+1` = 27, unchanged
from the previous batch) since it's still an accurate, meaningful number —
the hexdist between any two ADJACENT islands' centers in the CORRECT
tiling, confirmed identical for every ring (checked rings 1-11, always
exactly `27 * ring`) — just no longer used as a naive per-axis scale
anywhere. It's also still exactly right for `paint_margin_cell`'s existing
margin-bound formula (`SLOT_SPACING * (occupied_rings + 1)`), which needed
no change: confirmed the max fine-hexdist reach of ring N is exactly `27*N`
for every ring checked, not just the 6 axis corners.

Applied identically in both `server/src/lib.rs` (`geometry` module,
`SLOT_U`/`SLOT_V`/`SLOT_DET` consts) and `client/src/world.rs` (mirrored
exactly, same names/values) — client's now-fully-unused `SLOT_SPACING`
constant (nothing left references it there; the server still uses it for
the margin bound, a server-only concern) removed rather than left dead.

**VERIFIED**: Python prototype (not just the Rust code) checked slots 0..800
for duplicate coarse assignments (zero), slots 0..200's resulting island
regions for cell-level overlaps (zero) and gaps in a large inner window
(zero), and the inverse-lookup formula against 19,881 brute-force-checked
fine cells (zero mismatches) — all before writing a line of Rust, given how
much worse a wrong geometry rewrite would be than the cosmetic gap issue it
replaces. Both `cargo build -p server` and the client (native + web) build
clean. Republished the local instance, regenerated bindings (no schema
change). Screenshotted 6 real islands (4 real browser connections, freshly
seeded so they landed in the first ring around slot 0) at a zoomed-out view:
flat sides touching flat sides, no visible gap anywhere, no overlap —
matches the author's ask exactly.

---

**Previous batch:** F9.5 (author feedback, live during item 9's hand-testing)
— islands felt too far apart; author wants them closer, side by side, with
only small triangular gaps at the 3-way meeting points (classic hex-packing
look) instead of the ~3-tile margin band `SLOT_SPACING` originally left.

**Fix**: `SLOT_SPACING` 29 -> 27 (`2 * ISLAND_RADIUS + 1`, a 1-tile margin),
mirrored exactly in both `server/src/lib.rs` and `client/src/world.rs`
(unchanged: neither geometry formula shape, just the constant). Re-ran the
item-9 verification script with the new value: still zero duplicate
coarse-cell assignments across slots 0..500, minimum world-hexdist between
any two islands' centers is now 27 (was 29), still safely above `2 *
ISLAND_RADIUS` (26) — no overlap introduced, the margin just shrank from 3
tiles to 1. Republished the local instance, regenerated bindings (no schema
change, `module_bindings/` came out byte-identical), rebuilt both clients
clean.

---

**Previous batch:** F9.5 item 9 — island placement retest (ex-F6.5 task 2).
The original report predates the F2-F6 geometry rewrites; plan.md's own
instruction is to retest fresh and only chase render-side causes if it still
reproduces.

**Checked mathematically** (not just "ruled out by inspection" — actually
computed): a standalone replica of `server::geometry::slot_coords` /
`world::slot_coords` (both must match exactly; verified they do by reading
both side by side, unchanged this batch), run for slots 0..500 — zero
duplicate coarse-cell assignments (every slot gets a genuinely distinct
position, the hex-spiral algorithm itself has no bug), and the MINIMUM
world-space hexdist between any two islands' centers across that whole
range is exactly `SLOT_SPACING` (29), comfortably more than the `2 *
ISLAND_RADIUS` (26) needed to guarantee no two islands' 547-cell interiors
can ever overlap — a 3-unit margin gap always holds, by construction, for
every slot the spiral will ever produce, not just the 17 currently live on
the local instance (cross-checked against those 17 real slots too: no
duplicates there either).

**Not done**: a live visual re-check on the actual deployed build (culling
pop-in padding, stale/duplicate subscription rows are render-side concerns
the math above can't rule out) — this is explicitly the author's own
retest per plan.md's wording, and the author is doing their own hand-testing
pass right now rather than having me script another browser session for it.
Leaving this unchecked in `known_bugs.md` until the author confirms visually;
the geometry itself is verified clean either way.

---

**Previous batch:** F9.5 item 7 redesign (author feedback, same day as the
original batch) — the F8-style big centered modal (backdrop dim, close
button, Like/Unlike button, clickable link row) was too heavy for something
that now opens on mere hover; author asked for "way simpler... glue to the
mouse and smaller... without closing button... looking like a tooltip".

**Redesign** (`ui.rs`): the two very different popup UIs that shared
`island_popup` are now visually split by `is_own`:
- Own island (via the deliberate "My Isle" footer click) keeps the original
  full modal — backdrop, close button, link edit field/Set button — since
  it's a real form, not a fleeting hover artifact.
- A foreign island's hover popup is now `draw_island_tooltip`: a small box
  glued near the cursor (offset so it doesn't sit under it, flipped to the
  other side near screen edges), no backdrop, no close button (closes
  itself on hover-out, unchanged from the original batch), and — this is
  the important behavioral change, not just visual — NO buttons at all.
  Reasoned through why: a box that re-centers on the mouse every frame can
  never contain a clickable target, because moving the mouse toward the
  button moves the button the same distance; a real tooltip is
  non-interactic by definition anyway.

That drops two interactions the F8 popup used to host: the Like/Unlike
button (redundant — double-click-to-like on the map already does the exact
same thing, untouched) and the link row's click-to-open (which was NOT
redundant — this is the F9 itch.io-rate-traffic feature). Rather than lose
that quietly, asked the author directly: their pick was to move it onto a
plain single (non-double) click on the island itself, which fires alongside
the existing tap-opens-info behavior touch already relies on (decision 17)
— so `main.rs`/`bin/web.rs`'s `pending_info_click` resolution now also opens
and credits the link (if set) when it resolves, on top of what it already did.

**Two regressions caught before handing this to the author, both from
`island_popup` interactions the redesign didn't originally account for**:
1. **Double-click-to-like silently stopped working.** Root cause:
   `map_input_allowed` (and the item-5 `suppress_map_until_release` latch)
   still gated on `island_popup.is_some()` at all — leftover from when EVERY
   island popup was a true modal. Once hover started opening popups
   asynchronously (independent of any click), simply hovering a foreign
   island now flipped `map_input_allowed` false, which severed the
   long-press/double-click gesture tracking entirely (`long_press` gets
   force-reset every frame `map_input_allowed` is false) — hovering to LOOK
   at an island silently disabled liking it. Fixed by narrowing both checks
   to `island_popup.as_ref().is_some_and(|p| p.is_own)` — only the real
   modal blocks map input now; the tooltip never does.
2. **"My Isle" sometimes opened a STRANGER'S tooltip instead of your own
   popup.** Root cause: hovering is purely screen->world position based,
   and the footer/header bands' screen coordinates still map to SOME world
   tile through the camera transform even though they're visually covered by
   UI chrome — clicking "My Isle" (screen position over the footer) could
   have that same resting mouse position simultaneously satisfy the hover
   accumulator for whatever foreign island happens to occupy that world
   position at the camera's current framing, overwriting the just-opened
   own popup within the same second. Fixed by excluding the header/footer
   bands (`ui::HEADER_H`/`FOOTER_H`) from the hover computation entirely.

**VERIFIED** both regressions via scripted Playwright reproduction against
local `spacetime start` BEFORE fixing (confirmed both actually reproduced,
not just theorized) and re-confirmed fixed after: screenshotted "My Isle"
opening correctly and staying open (owner name in the popup now matches the
clicking player's own footer name field, where before the fix it showed a
different player's name entirely). Both clients build clean. Did not
re-verify the double-click fix with a full two-window capture (same
far-apart-islands framing difficulty noted in items 6/7's original
verification) — traced by hand instead (REASONED) after confirming the
root-cause mechanism directly (the stale `island_popup.is_some()` gate).
Handing off to the author for real hand-testing now rather than continuing
to script increasingly elaborate multi-window browser repros.

---

**Previous batch:** F9.5 item 8 — Safari (macOS) trackpad scroll zooms in
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
- [x] F9 — island links (jam rate id only), link-click XP, time XP —
      implemented (see the batch notes above); server-side dedupe/guards
      VERIFIED via CLI, client wiring + the 60s time-XP tick author
      hand-tested and confirmed working (2026-07-11). COMMITTED.
- [x] F9.5 — post-deploy bug sweep (10 items: account import bug, FPS at
      scale, merge range, recent-colors, modal click-through, cursor scale,
      island-info-on-hover redesign, Safari wheel-zoom clamp, zero-gap ->
      gapped tiling, dead-player reap) — all VERIFIED across their
      individual batches above. COMMITTED.
- [x] F9.6 — UX polish batch (8 items: eraser, middle-click eyedropper,
      heart icon, color-overlay ergonomics, help overlay, keyboard/right-
      drag camera, launch intro, borderless far zoom) — see the batch notes
      above. Eraser reducers VERIFIED live; everything else REASONED (both
      targets build clean, web release build succeeds, package size fine).
      Author hand-test pass complete (2026-07-11): one real click-through bug
      found and fixed (Colors button self-closing, see the batch above),
      footer layout and two constants (`INTRO_DURATION`,
      `BORDERLESS_ZOOM_THRESHOLD`) re-tuned, author confirmed the rest feels
      good. COMMITTED.

## P2 (only if time remains)
- [x] F10 admin — `claim_admin`/`set_frozen`/`delete_island_cells` reducers
      (SHA-256-gated password, freeze toggle enforced on all 17
      player-facing mutating reducers, moderation cell-wipe), admin island
      relocated to slot 0 on claim, ops docs + backup dump commands in
      `WORK.md`. VERIFIED live end-to-end via a throwaway WebSocket probe +
      `spacetime sql` (see the batch notes above). No client UI — CLI-only
      per plan.md's "admin tooling is minimal for now." NOT yet published
      to the production VPS (same access gap as the border feature above).
      COMMITTED.
- [x] F11 flying gift — scheduled spawn/expire tick, drifting world-space
      pickup, click/tap `claim_gift` (distance-checked against the CURRENT
      drifted position) grants a fresh hue or flat XP. VERIFIED live via a
      throwaway WebSocket probe (see the batch notes above); client
      rendering/gesture REASONED, not hand-tested by the executor. NOT
      committed — author hand-test pending per this repo's convention.
- [ ] F12 polish/bots/sounds — all 4 plan.md sub-parts addressed
      (2026-07-11): (1) bots adapted to world-cartesian coordinates + new
      "Merge with me!" center bot + cursor name labels — VERIFIED live (bot
      positions via `spacetime sql`), name-label rendering REASONED; (2)
      help overlay now explains the merge mechanic, not just keybindings —
      REASONED; (3) sound effects (4 CC0 sfx wired to the existing
      merge/gift/levelup/error toasts, embedded via `include_bytes!`, both
      targets) — build-level REASONED, audible playback not verified (no
      speakers in this environment); (4) itch page copy drafted
      (`itch-page.md`) — not itself verifiable, needs the author to paste it
      into itch.io and add screenshots. See the batch notes above for all
      four. NOT committed — full author hand-test pass (including actually
      hearing the sounds) still pending, same convention as every prior
      batch.
- [ ] F13 hexa event (6-cursor hexagon: pooled dictionaries, one-time XP, snap
      rendering) — IMPLEMENTED (2026-07-11): server trigger/pooling/one-time-XP
      VERIFIED live via a throwaway 6-identity probe (`spacetime sql`: exactly
      6 `hexa_reward` rows, exactly 1 `hexa_event` row, idempotent on
      re-trigger); both clients' hexagon-vertex-snap rendering + ignition
      toast REASONED, not hand-tested (`cargo build`/`./build-web.sh` clean).
      Follow-up (2026-07-11, author feedback): cluster detection moved
      SERVER-side (`hexa_cluster` table + `hexa_sweep` safety net) instead of
      each client guessing its own, and the local player's own cursor now
      snaps to its hexagon slot too, not just other players'. VERIFIED live
      (cluster id/vertex stability, shrink-on-departure, sweep cleanup); both
      clients' rendering REASONED only, same as the first pass. See the
      batch notes at the top of this file. NOT committed — full author
      hand-test pass pending, same convention as every prior batch.
- [x] Customizable island border color/transparency (from backlog, author override
      2026-07-11) — server + both clients VERIFIED via `spacetime call`/`spacetime sql`
      against the local instance; client rendering (including the follow-up
      Shown/Hidden toggle + swatch preview) author hand-tested and confirmed
      working (2026-07-11). NOT yet published to the production VPS — that
      publish requires VPS shell access this environment doesn't have; the
      author will run it (see the commands left in the prior turn). See the
      batch notes at the top of this file and plan.md's backlog entry.
- [ ] F14 community island (playground), named "Free Isle" — author ruling
      2026-07-11 (decision 20): slot 0 is now a permanent, ownerless canvas
      anyone can paint/erase; `claim_admin` no longer relocates islands to
      it. Its info popup/hover-tooltip shows "Free Isle" (never "another
      player"), and its unpainted tiles render white instead of the usual
      gray. Server mechanic VERIFIED live via a throwaway 2-identity probe
      (`spacetime sql`: the slot-0 row exists with the `Identity::ZERO`
      sentinel owner, a non-owner painted a cell, a DIFFERENT non-owner
      erased it, an out-of-bounds paint was rejected). Client dispatch +
      naming + white fill REASONED, not hand-tested (`cargo build`/
      `./build-web.sh` clean). See the batch notes at the top of this file.
      NOT committed — author hand-test pass pending, same convention as
      every prior batch; also not yet published
      to the production VPS.

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
- **F10 follow-up (2026-07-11): admin password moved from a hand-pasted hash
  to a build-time env var.** Author lost track of the plaintext behind the
  originally-committed `ADMIN_PASSWORD_SHA256` literal and could no longer
  claim admin. `server/build.rs` now reads `ADMIN_PASSWORD` (from the
  process env, or a gitignored `server/.env` — parses simple `KEY=value`
  lines itself, no `dotenvy` dependency added) and emits it hashed via
  `cargo:rustc-env=ADMIN_PASSWORD_SHA256=...`; `constants::ADMIN_PASSWORD_SHA256`
  in `server/src/lib.rs` is now `env!("ADMIN_PASSWORD_SHA256")` instead of a
  literal. `server/.env.example` is the committed template; `.env` was added
  to the root `.gitignore`. To change the admin password going forward: edit
  `server/.env`, rebuild, republish — no more manual SHA-256'ing by hand.
  **Behavior change to flag**: a fresh clone can no longer build `server`
  until `.env` exists with `ADMIN_PASSWORD` set (build.rs panics with a
  message pointing at `.env.example` otherwise) — previously any clone built
  out of the box since the hash was hardcoded. VERIFIED: `cargo build -p
  server --target wasm32-unknown-unknown` succeeds with `server/.env`
  present and fails with the intended message when it's absent; rest of the
  workspace (`client`'s native `client`/`bot` bins, `shared`) still builds
  clean — `client`'s `web` bin still fails on the default host target, but
  that's the pre-existing, intentional emscripten-only exception (target-gated
  `serde_json` dep in `client/Cargo.toml`), not caused by this change. A new
  random password was generated into `server/.env` (not committed) so the
  author has a working admin login again; not itself hand-tested by
  `claim_admin` against a live instance this session — only the build/hash
  pipeline was verified.

---

**Batch (2026-07-12, pre-submission session): F14 community-island fixes +
web zero-identity bug + web build hygiene.** Three author asks plus a
handoff from a previous session (`HANDOFF-web-native-sharing.md`, now
absorbed and deleted). See plan.md's F14 entry for the author-reversal and
bug-fix writeups; this is the evidence log.

- **Web zero-identity normalization bug** — ROOT CAUSE VERIFIED: a raw
  SQL-API dump of the slot-0 row showed `[["0x0"],1,0]` — the wire's minimal-
  hex encoding never produces the full 64-zero form web's old
  `normalize_identity` expected `COMMUNITY_OWNER_HEX` to match. FIX VERIFIED:
  moved normalization into shared `client/src/world.rs::normalize_identity_hex`
  (strip `0x`, lowercase, left-pad to 64 chars); added `#[cfg(test)]` tests
  (`"0x0"` -> 64 zeros; a full-width mixed-case value -> lowercased, unchanged
  length) — `cargo test -p client --bin client` passes both. `web.rs`'s
  `normalize_identity` now delegates to it.
- **Shared decision helpers** — `world::COMMUNITY_OWNER_HEX` (moved from
  web.rs) and `world::unpainted_island_fill(is_community: bool) -> Color`
  (replaces the mirrored white-vs-gray blocks in `main.rs`/`web.rs`) — part
  of the standing "share, don't hand-mirror" directive.
- **Hover/like disabled on the community island** (author reversal — F14
  originally spec'd the popup/hover/like to work on it like any foreign
  island; the sentinel owner passed every `owner != me` filter, which the
  author decided reads wrong for an ownerless playground). VERIFIED:
  native (`main.rs`) `info_target` and `currently_hovered_foreign` filters
  gained `&& island.owner != Identity::ZERO`; web (`web.rs`) the equivalent
  `owner_hex != world::COMMUNITY_OWNER_HEX` guard on both. Server backstop
  in `like_island`/`unlike_island` (`server/src/lib.rs`) rejects
  `island.owner == Identity::ZERO` — VERIFIED live: `spacetime call -s local
  hexmerge like_island 1` returns `"cannot like the community island"`
  after republish.
- **Web build hygiene** (also resolves the "hearts don't show on web"
  symptom, which traced to a stale/uncached wasm build, not a code bug —
  heart rendering already lived in shared `ui.rs`): added a `BUILD_VERSION`
  cache-buster to `client/web/game.html` — a version query param on the
  `web.js` script tag (rewritten via `document.write` since the tag needs
  the JS variable) and a `Module.locateFile` override appending the same
  version to the `web.wasm` fetch. VERIFIED: `./build-web.sh` clean rebuild,
  `web.js`/`web.wasm` timestamps confirm the fresh copy landed in
  `client/web/`.
- **One-time local DB cleanup** (366 leftover painted cells from
  pre-cleanup testing, persisted across republishes since
  `--delete-data=never`): `DELETE FROM island_cell WHERE island_id = 1`,
  `DELETE FROM island_like WHERE island_id = 1`, `UPDATE island SET likes =
  0 WHERE id = 1` (island id 1 confirmed = slot 0 via `spacetime sql`).
  VERIFIED: `SELECT COUNT(*) AS n FROM island_cell WHERE island_id = 1` -> 0
  after; `likes` column -> 0 after.
- **Builds**: `cargo check -p server`, `cargo build -p client --bin client
  --bin bot`, `cargo test -p client --bin client`, and `./build-web.sh` all
  clean (VERIFIED) after every code change in this batch, including after
  the local republish (`./server/publish.sh`, `--delete-data=never`
  confirmed again, data survived).
- NOT hand-tested in a GUI/browser this session (executor protocol — see
  memory) — the author needs to hard-reload `localhost:8080` and confirm:
  hearts render, community island all white, no hover tooltip / double-click
  like on it in either client, erase still whitens it, hover/like still work
  on ordinary foreign islands. `HANDOFF-web-native-sharing.md` deleted now
  that this entry supersedes it.

---

**Batch (2026-07-12, submission day, chat session): F15 title screen —
"hexel" hexagon wordmark + animated Draw button.** Author chat request
("Create a nice hexagon typography for hexel", lowercase `h` explicit; then
a title screen "with the title in big, an animated button 'Draw' and a semi
transparent background, showing a hint of the whole map behind"). Full
design rationale in plan.md's F15 entry; client-only, no republish.

- **Wordmark** (`ui.rs` `TITLE_GLYPH_*`/`TITLE_WORD`/`draw_hexel_logo`):
  glyphs as const `(q, 2*v)` cell tables on the game's own flat-top axial
  grid, rendered with `world::draw_hex` + `world::hsv_color` (hue sweeps
  0->330 across the word, sat 70 / val ~88 with a traveling shimmer; own
  drop-shadow pass). Glyph shapes iterated visually offline (PIL renders
  of the exact cell tables + a 720x720 layout mock with the shipped
  constants) before porting — VERIFIED visually against those renders,
  which use the same flat-top vertex math as raylib's `draw_poly`.
- **Title state/input** (`ui.rs`): `UiState.title_active` starts true;
  `handle_input` swallows HUD input and clears the flag on a Draw-button
  click or Enter; `draw` renders backdrop `(8,9,14,205)` + wordmark +
  pulsing button instead of the HUD. `any_modal_open()` now includes the
  title (gates painting/panning/gift claims/hover popups/`set_pos`
  heartbeat through the existing chokepoints in both clients, REASONED by
  tracing every `any_modal_open`/`map_input_allowed` call site);
  `over_map_area` in both clients gains `!title_active` (computed before
  `handle_input` flips the flag, so the dismissing click can't paint the
  tile under the button — REASONED).
- **Camera** (`main.rs` + `bin/web.rs`, mirrored like the intro itself):
  while the title is up, hold the `world_fit` whole-world pose each frame;
  dismissing starts the existing F9.6 intro whose `intro_from` calls the
  same `world_fit`, so the ease into the own island continues from the held
  pose (REASONED).
- **Builds**: `cargo check -p server`, `cargo build -p client --bin client
  --bin bot`, `cargo test -p client --bin client` (8 passed), `cargo test
  -p server` (5 passed), `./build-web.sh` all clean, zero warnings
  (VERIFIED).
- NOT hand-tested in a GUI/browser (author drives runtime testing): needs
  an eyeball on backdrop alpha over a real painted map, shimmer/pulse feel,
  and that Draw -> intro ease reads as one continuous motion in both
  clients.
