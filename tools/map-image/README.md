# Title-map bake

`render_map.py` renders the painted world to `title-map.png` plus a
`title-map-view.txt` recording the camera pose and newest-paint timestamp it
was baked at. The title screen draws that PNG instead of rasterising ~35k
hexes a frame; the client fetches a fresh pair at runtime (see
`hexelLoadTitleMap` in `client/web/game.html`), so a re-bake reaches players
on their next page load with no rebuild.

`rebake_on_rerank.sh` re-renders only when the set of `(island id, slot)`
pairs changes — re-ranks, island creation, reaps — and no-ops on ordinary
painting.

## Why this needs a timer

Islands change slot on every leaderboard re-rank (`RERANK_PERIOD_SECS = 300`),
so an un-refreshed bake is wrong within minutes: the backdrop shows islands
where they used to be, which is plainly visible as the intro crossfades from
the baked image into the live map (`title_map::blend_alpha`, zoom 4 -> 8).

That is exactly what happened between 2026-07-26 and 2026-07-28. The watcher
script shipped, but nothing on the box ever ran it — no timer was installed
and `python3-pil` was missing, so the bake stayed frozen at the commit that
introduced it and drifted two days out of date.

## Install (per host)

```sh
sudo apt-get install -y python3-pil          # render_map.py's only dependency
cp tools/map-image/systemd/hexel-map-rebake.* ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now hexel-map-rebake.timer
loginctl enable-linger "$USER"               # so it ticks with nobody logged in
```

Verify:

```sh
systemctl --user list-timers hexel-map-rebake.timer
curl -s https://hexel.mister-esman.uk/title-map-view.txt   # 4th field = newest paint, in micros
```

Both unit files hardcode `/home/ubuntu/raylib-gamejam-2026` and
`/home/ubuntu/.local/bin` (where `spacetime` lives, and which a systemd unit
does not inherit from a login shell) — adjust if the checkout moves.

## By hand

```sh
./render_map.py hexel ../../client/assets/title-map   # render only
./tools/map-image/rebake_on_rerank.sh                 # render if the layout changed, then stage both copies
```

`client/assets/*` is the tracked, compiled-in fallback that ships inside the
wasm; `client/web/*` is the served copy the running client fetches, and is
gitignored precisely because the watcher rewrites it every few minutes.
