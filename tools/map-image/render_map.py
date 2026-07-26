#!/usr/bin/env python3
"""Render the painted world to a PNG for the title screen.

The title screen used to draw the live world behind its backdrop, which
means rasterising ~35k hexes every frame just to show a blurred map. This
bakes that view once, offline, so the title costs a single textured quad.
Live player cursors are still drawn on top by the client — the map is the
static part, the "life" is not.

Also writes `map_view.txt`: the world-space pose the image was rendered
with (center x/y and world units per pixel). The client sets its title
camera to exactly that pose, so cursors drawn through the normal camera
transform line up with the baked pixels.

Geometry mirrors client/src/world.rs — flat-top hexes of outer radius 1.0,
`axial_to_world` = (1.5q, sqrt(3)(r + q/2)), islands placed on the coarse
`slot_coords`/`slot_center` lattice. Keep in sync if those change.

    ./render_map.py hexel ../../client/assets/title-map
"""

import colorsys
import json
import math
import os
import re
import sys
import urllib.request

from PIL import Image, ImageDraw

HOST = "http://127.0.0.1:3000"
# The game window is 720x720, so the camera zoom written to `-view.txt` is
# always relative to SCREEN. IMAGE_SIZE is independent: rendering the same
# world extent at a higher resolution just means the texture stays sharp when
# the intro zooms into it, without changing the framing.
SCREEN = 720
IMAGE_SIZE = 2160
SUPERSAMPLE = 2     # rendered at 2x then box-filtered, so hex edges aren't jagged
MARGIN_FRAC = 0.02  # breathing room so edge hexes aren't clipped
BACKGROUND = (18, 18, 24, 255)  # matches main.rs clear_background

# --- geometry, mirrored from client/src/world.rs -----------------------------
ISLAND_RADIUS = 15
MARGIN_GAP_TILES = 8
SLOT_PLACEMENT_RADIUS = ISLAND_RADIUS + MARGIN_GAP_TILES // 2
SLOT_U = (SLOT_PLACEMENT_RADIUS, SLOT_PLACEMENT_RADIUS + 1)
SLOT_V = (-(SLOT_PLACEMENT_RADIUS + 1), 2 * SLOT_PLACEMENT_RADIUS + 1)
DIRECTIONS = [(1, 0), (1, -1), (0, -1), (-1, 0), (-1, 1), (0, 1)]
SQRT3 = math.sqrt(3.0)


def slot_coords(slot_index):
    """slot index -> coarse axial (q, r); slot 0 at the origin, then spiralling."""
    if slot_index == 0:
        return (0, 0)
    remaining, ring = slot_index, 1
    while remaining > 6 * ring:
        remaining -= 6 * ring
        ring += 1
    dq, dr = DIRECTIONS[4]
    q, r = dq * ring, dr * ring
    steps_left = remaining - 1
    for dq, dr in DIRECTIONS:
        for _ in range(ring):
            if steps_left == 0:
                return (q, r)
            q, r, steps_left = q + dq, r + dr, steps_left - 1
    return (q, r)


def slot_center(q, r):
    return (q * SLOT_U[0] + r * SLOT_V[0], q * SLOT_U[1] + r * SLOT_V[1])


def axial_to_world(q, r):
    return (1.5 * q, SQRT3 * (r + q / 2.0))


# --- database ---------------------------------------------------------------
def cli_token():
    src = open(os.path.expanduser("~/.config/spacetime/cli.toml")).read()
    return re.search(r'spacetimedb_token = "([^"]+)"', src).group(1)


def query(db, sql, token):
    req = urllib.request.Request(
        f"{HOST}/v1/database/{db}/sql",
        data=sql.encode(),
        headers={"Authorization": f"Bearer {token}"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)[0]["rows"]


def rgb(packed):
    """Decode a cell's packed colour to RGBA.

    `color` packs HSV, NOT RGB — mirrors `world::unpack_hsv`/`hsv_color`:
    hue in bits 16..24 (0..359), then saturation and value as percentages.
    Reading it as 0xRRGGBB turns the map's pastels into dark maroon.
    """
    v = int(packed)
    h = (v >> 16) & 0x1FF
    s = ((v >> 8) & 0xFF) / 100.0
    val = (v & 0xFF) / 100.0
    r, g, b = colorsys.hsv_to_rgb((h % 360) / 360.0, min(s, 1.0), min(val, 1.0))
    return (round(r * 255), round(g * 255), round(b * 255), 255)


def main():
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <db> <output-basename>")
    db, out_base = sys.argv[1], sys.argv[2]
    token = cli_token()

    slots = {int(r[0]): int(r[1]) for r in query(db, "SELECT id, slot FROM island", token)}
    cells = []  # (world_x, world_y, color)
    newest_paint = 0  # max painted_at micros across everything baked in

    def paint_micros(value):
        # A Timestamp comes back wrapped differently depending on the encoding
        # (bare int, {"__timestamp_micros...": n}, or a single-element array),
        # so unwrap containers until something converts to an int.
        for _ in range(4):
            if isinstance(value, dict):
                value = next(iter(value.values()))
            elif isinstance(value, (list, tuple)):
                if not value:
                    return 0
                value = value[-1]
            else:
                break
        try:
            return int(value)
        except (TypeError, ValueError):
            return 0

    for island_id, q, r, color, painted_at in query(
        db, "SELECT island_id, q, r, color, painted_at FROM island_cell", token
    ):
        slot = slots.get(int(island_id))
        if slot is None:
            continue  # cell whose island vanished; nothing to place it against
        cx, cy = slot_center(*slot_coords(slot))
        cells.append((*axial_to_world(cx + int(q), cy + int(r)), rgb(color)))
        newest_paint = max(newest_paint, paint_micros(painted_at))
    n_island = len(cells)
    # margin_cell coordinates are already world-absolute, not island-local.
    for q, r, color, painted_at in query(
        db, "SELECT q, r, color, painted_at FROM margin_cell", token
    ):
        cells.append((*axial_to_world(int(q), int(r)), rgb(color)))
        newest_paint = max(newest_paint, paint_micros(painted_at))
    print(f"{n_island} island cells + {len(cells) - n_island} margin cells")
    if not cells:
        sys.exit("no painted cells to render")

    # Square, centred fit: one scale for both axes keeps hexes regular, and a
    # square output means the client can stretch it to the 720x720 screen.
    xs = [c[0] for c in cells]
    ys = [c[1] for c in cells]
    cx = (min(xs) + max(xs)) / 2.0
    cy = (min(ys) + max(ys)) / 2.0
    extent = max(max(xs) - min(xs), max(ys) - min(ys)) / 2.0 + 1.0  # +1 hex radius
    extent *= 1.0 + MARGIN_FRAC

    res = IMAGE_SIZE * SUPERSAMPLE
    px_per_world = res / (2.0 * extent)
    img = Image.new("RGBA", (res, res), BACKGROUND)
    draw = ImageDraw.Draw(img)
    corners = [(math.cos(k * math.pi / 3.0), math.sin(k * math.pi / 3.0)) for k in range(6)]
    for wx, wy, color in cells:
        sx = (wx - cx) * px_per_world + res / 2.0
        sy = (wy - cy) * px_per_world + res / 2.0
        draw.polygon(
            [(sx + dx * px_per_world, sy + dy * px_per_world) for dx, dy in corners],
            fill=color,
        )

    img = img.resize((IMAGE_SIZE, IMAGE_SIZE), Image.LANCZOS)
    png_path = f"{out_base}.png"
    os.makedirs(os.path.dirname(os.path.abspath(png_path)), exist_ok=True)
    img.save(png_path, optimize=True)

    # Camera pose for the client: zoom is relative to the 720px SCREEN, not
    # IMAGE_SIZE, so bumping the render resolution sharpens the texture
    # without shifting the framing. The fourth field is the newest paint the
    # bake contains — the client draws anything painted after it live on top,
    # so fresh art still shows up on the zoomed-out view.
    with open(f"{out_base}-view.txt", "w") as fh:
        fh.write(
            f"{cx:.6f} {cy:.6f} {SCREEN / (2.0 * extent):.6f} {newest_paint}\n"
        )

    print(f"wrote {png_path} ({os.path.getsize(png_path) // 1024} KiB)")
    print(f"wrote {out_base}-view.txt  target=({cx:.2f}, {cy:.2f}) zoom={SCREEN / (2.0 * extent):.4f} newest_paint={newest_paint}")


if __name__ == "__main__":
    main()
