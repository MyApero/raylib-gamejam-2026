# Hexaworld — itch.io page draft (F12: "page styling on itch")

Paste/adapt this into the itch.io project page's Description field (itch's
editor supports headers, bold, links, and images — this file uses plain
Markdown as the closest portable approximation; re-apply formatting by hand
in itch's editor, it does not import `.md` directly). This is copy/content
only — actually editing the live itch.io page needs the author's itch.io
login, which this environment doesn't have.

---

## Hexaworld

**One shared canvas. Every player paints their own hex island — and when
two cursors touch, your colors merge into something neither of you had
before.**

Made for the raylib 6.x gamejam, theme **hex + merge**.

### The idea

Everyone starts with one random color. That's it — that's your whole
palette. The only way to get more is to **merge**:

- Touch cursors with another player and your brushes blend into a brand
  new hue. You both keep it.
- Playing solo? Long-press any painted tile instead and take a merge of
  your brush with whatever's already there.

Every new color is XP. XP raises how saturated you're allowed to paint.
Paint your own island (and the shared margins between islands) with
whatever you've unlocked so far — the whole map is one persistent world,
live, all jam long.

### How to play

- **Left-drag** on your island or the margin: paint
- **Long-press** a foreign tile: merge / take its color
- **Middle-click** a painted tile: eyedropper (must already be unlocked)
- **Double-click** a foreign island: like it
- **Hover** a foreign island: see who made it
- Mouse wheel / pinch to zoom, drag to pan — works on touch too

Full controls are one Escape-press away in-game.

### Also playable at

**https://raylib.mister-esman.uk** — same live world either way, so it
doesn't matter which entry point you (or your friends) use.

### Built with

raylib 6.0 + SpacetimeDB. Source: (repo link).

---

Notes for the author, not for the page itself:
- Add 2-4 screenshots/a short clip once a few islands exist on the live
  world — an empty map undersells it. A GIF of an actual merge happening
  (two cursors touching, the toast, the color change) is the single most
  convincing thing this game can show in a still image.
- itch upload settings per plan.md's F7: "played in browser", viewport
  exactly 720x720, fullscreen button enabled, mobile-friendly flag set.
- Cover image: itch wants ~630x500 or 315x250; a cropped screenshot of a
  cluster of colorful islands reads better than any text-only cover.
