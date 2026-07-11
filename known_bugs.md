- [x] merged with ... shouldn't include the id of someone
id is very confidential as it can be used to take the account (resolved at F6:
random names at first connect, `player_label` never falls back to an identity
hex — see status.md)

- [ ] When launching, the selected color is not in the recent color used

- [x] You can't paste in "Paste an ID" (resolved at F6: Ctrl+V/Cmd+V reads
`get_clipboard_text()` into the import field — see status.md)

- [x] reset an account should give a random color (resolved at F6: `reset_account`
rolls a fresh `ctx.rng()` hue instead of the deterministic `start_hue` — see status.md)

- [ ] Ilot aren't correctly placed in the world

- [ ] A player that creates an ilot by connecting and disconnects just after without doing a single "action" (modifying a cell, merging) should be considered as dead and he should be removed from the world after a while (like 5 minutes). His ilot would be removed and his user too (that would reduce the total number of players).

- [ ] 40FPS with 10 ilots? What's happening already, it should be smoother. 30 fps with 18 max players.

- [ ] Scroll on macos (safari) is bugged

- [ ] Ilot info should only be hover

- [ ] Dezoom should reduce the size of other's cursor

- [ ] More range to merge

- [x] Import account doesn't work, it creates another one (F9.5 item 1: the
import field's 256-char cap silently truncated real ~386-char reconnect
tokens into a corrupt JWT, which the server rejected — see status.md)

- [ ] reset account doesn't reset the last 3 selected colors
