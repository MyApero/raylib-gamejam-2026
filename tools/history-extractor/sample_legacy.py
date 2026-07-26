#!/usr/bin/env python3
"""Turn a full legacy working file into the one the emit pass merges.

The recovered log from the seeded-from database holds ~19.5M events, which is
316 MB in the shipped format and cannot go in a web bundle. Nearly all of it
belongs to one island: the shared canvas at the world centre, which every
player paints on, accounts for over 99% of all paint events on its own. Every
other island's entire history fits in about 1.5 MB.

So the cap is per island rather than global. An island under the cap keeps
every stroke it ever received — its timelapse is exact, which is the whole
point of an island export. Only an island above the cap is thinned, evenly
across its own history so it still spans the full period.

Margin paints, deletes and island placement events are always kept whole:
between them they are a few tens of thousands of events, and deletes in
particular cannot be dropped without leaving erased cells on screen forever.

    ./sample_legacy.py legacy-raw.bin output/hexel-legacy-history.bin [cap]
"""

import struct
import sys
from collections import Counter

MAGIC = b"HEXELRAW\x01"
RECORD = 33
ISLAND_AT = 9
SHIPPED_RECORD = 17
# Chosen from the measured distribution: every island except the shared canvas
# is complete well below this, so raising it further only buys detail on the
# one island whose history is chaotic rather than composed.
DEFAULT_CAP = 100_000
# Paint inserts. Kind 0/2 (deletes), 3 (margin paint) and 4/5 (island
# placement) are never thinned — see the module docstring.
ISLAND_PAINT = 1


def read_blocks(path):
    with open(path, "rb") as handle:
        if handle.read(len(MAGIC)) != MAGIC:
            raise SystemExit(f"{path}: not a history working file")
        while True:
            buf = handle.read(RECORD * 200_000)
            if not buf:
                return
            yield buf


def main():
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    cap = int(sys.argv[3]) if len(sys.argv) > 3 else DEFAULT_CAP

    totals = Counter()
    for buf in read_blocks(src):
        for i in range(0, len(buf) - RECORD + 1, RECORD):
            if buf[i] == ISLAND_PAINT:
                totals[struct.unpack_from("<I", buf, i + ISLAND_AT)[0]] += 1

    # One stride per island: 1 keeps everything, which is the case for all but
    # the shared canvas.
    strides = {
        island: max(1, -(-count // cap)) for island, count in totals.items()
    }
    thinned = sorted(i for i, s in strides.items() if s > 1)

    seen = Counter()
    kept = 0
    with open(dst, "wb") as out:
        out.write(MAGIC)
        for buf in read_blocks(src):
            for i in range(0, len(buf) - RECORD + 1, RECORD):
                if buf[i] == ISLAND_PAINT:
                    island = struct.unpack_from("<I", buf, i + ISLAND_AT)[0]
                    stride = strides[island]
                    if stride > 1:
                        index = seen[island]
                        seen[island] = index + 1
                        if index % stride:
                            continue
                out.write(buf[i : i + RECORD])
                kept += 1

    print(f"islands={len(totals)} cap={cap}")
    print(f"complete_islands={len(totals) - len(thinned)}")
    for island in thinned:
        print(f"thinned island={island} events={totals[island]} stride={strides[island]}")
    print(f"kept_events={kept} shipped_bytes~={kept * SHIPPED_RECORD}")


if __name__ == "__main__":
    main()
