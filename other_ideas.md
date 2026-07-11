- [x] le cursor devrait grossir à un certain niveau de zoom, pour moi il devrait render environ la taille d'une tôle si on zoom assez
(F9.5 item 6: other players' cursors now scale with camera zoom relative to
their size at the default ISLAND_FIT_ZOOM, floored so they stay findable
zoomed far out — see status.md)
- [x] hover to see island information (F9.5 item 7 / decision 17 — see status.md)
- [x] heart icon for like
(F9.6 item 3: heart icon replaces the old "Likes: N" text in both the
island popup and the hover tooltip, filled = already liked — see status.md)
- [x] hover to see ilot information (duplicate of the entry above — F9.5 item 7)
- [x] Eraser
(F9.6 item 1: erase_island_cell/erase_margin_cell reducers, X key + footer
toggle, verified live — see status.md)
- [x] Quand on lance le jeu, intro vue d'ensemble de la map, zoom sur ton île
(F9.6 item 7: camera eases from a whole-world view to your island; see the
"Slow then fast" entry below though — the easing curve may be backwards)
- [x] Move with arrow keys
(F9.6 item 6 — see status.md)
- [x] Move with WASD and zoom with QE
(F9.6 item 6 — see status.md)
- [x] Move with Right click and drag
(F9.6 item 6 — see status.md)
- [x] Key for Key bindings (echap?)
(F9.6 item 5: Escape toggles a minimal controls overlay when nothing else
is open — see status.md)
- [x] Middle click to take the color you're looking at
(F9.6 item 2 — see status.md)
- [x] Sort Colors by Hue in Colors interface
(F9.6 item 4 — see status.md)
- [x] Echap quit the Colors interface or click outsite
(F9.6 item 4, plus a real click-through regression the author's own
hand-test caught and fixed same-day — see status.md)
- [x] Color interface, hover should show the HEX code
(F9.6 item 4 — see status.md)
- [x] At a certain zoom level, remove the tile black border to make it look more like a painting
(F9.6 item 8, "borderless far zoom" — see status.md)
- [x] Click on close for color modal shouldn't click on the tile behind
(F9.5 item 5: a click's press and release span several frames — the overlay
closes on the press frame, but the button stays down for the rest of the
click, and every one of those frames used to see "no modal" and let the SAME
press paint. Latches at press-start for the whole gesture instead; see
status.md)


- [ ] Centre Ilot should have a bot drawing R and a heart
- [ ] It should also be a battlefield
- [ ] Outsite it should be only margin, if someone wants to draw a biiiiig thing
- [ ] Having more XP gives more Ilots?
- [ ] Bot at the middle with a color and a highlight "Merge with me!"
- [ ] Customise your Isle border color or make it transparent (remove)

- [ ] Slow then fast for the start animation (easeInOutCirc)
(NOT done as requested — the launch intro I built (F9.6 item 7) eases
OUT: fast at the start, slowing into the landing on your island. This item
asks for the opposite (slow start, accelerating finish). Flagging rather
than checking it off; swap `1.0 - (1.0 - t).powi(3)` for `t.powi(3)` in
both `main.rs`/`bin/web.rs` if that's still wanted.)
- [ ] Better icon for the eraser icon
(Partially addressed, not by me: the author's own follow-up commit
`cc6a47b` replaced the FOOTER button's text label with a proper two-tone
eraser glyph. The cursor-side badge shown near the mouse while erasing
(`world::draw_eraser_badge`) is still my original placeholder circle — may
be what this item is actually about.)
- [ ] When Locked, border of the cursor should be thicker
- [ ] Leaderboard should take like as a first metric and also number of tiles drawn in his ilot
- [ ] white border around the tile you hover should act like the ilot border, always visible even when zoomed out
- [ ] Put a clear hexel title with a ENTER
