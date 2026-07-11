- [x] merged with ... shouldn't include the id of someone
id is very confidential as it can be used to take the account (resolved at F6:
random names at first connect, `player_label` never falls back to an identity
hex — see status.md)

- [x] When launching, the selected color is not in the recent color used
(F9.5 item 4: `UiState::seed_last3_once` seeds the footer ring with the
caller's current hue the first frame it's known — see status.md)

- [x] You can't paste in "Paste an ID" (resolved at F6: Ctrl+V/Cmd+V reads
`get_clipboard_text()` into the import field — see status.md)

- [x] reset an account should give a random color (resolved at F6: `reset_account`
rolls a fresh `ctx.rng()` hue instead of the deterministic `start_hue` — see status.md)

- [ ] Ilot aren't correctly placed in the world

- [ ] A player that creates an ilot by connecting and disconnects just after without doing a single "action" (modifying a cell, merging) should be considered as dead and he should be removed from the world after a while (like 5 minutes). His ilot would be removed and his user too (that would reduce the total number of players).

- [x] 40FPS with 10 ilots? What's happening already, it should be smoother. 30 fps with 18 max players.
(F9.5 item 2: both clients rebuilt a full HashMap from every island_cell row
in the world every frame, regardless of view culling — replaced with O(1)
point lookups by packed cell id, cost now proportional to in-view cells only;
see status.md)

- [ ] Scroll on macos (safari) is bugged

- [x] Ilot info should only be hover
(F9.5 item 7 / decision 17: popup now opens after a 200ms continuous hover
over a foreign island, closes on hover-out; double-click-to-like and touch
tap-to-open are unchanged — see status.md)

- [x] Dezoom should reduce the size of other's cursor
(F9.5 item 6: `world::draw_cursor_scaled` — see status.md)

- [x] More range to merge (F9.5 item 3: `MERGE_DIST` 1.0 -> 2.0,
`server/src/lib.rs`; plan.md's constants table already carried the settled
value — author should re-tune further by feel on the deployed build if 2.0
still isn't right)

- [x] Import account doesn't work, it creates another one (F9.5 item 1: the
import field's 256-char cap silently truncated real ~386-char reconnect
tokens into a corrupt JWT, which the server rejected — see status.md)

- [x] reset account doesn't reset the last 3 selected colors
(F9.5 item 4: `UiState::note_reset_hue` clears the ring before reseeding with
the fresh post-reset hue, instead of just prepending onto stale pre-reset
entries — see status.md)
