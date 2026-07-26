#!/usr/bin/env python3
"""Convert a v3 recovered-history artifact to the v4 format the client reads.

v3 records are 25 bytes:

    kind u8 | tx_offset u64 | island_id u32 | q i32 | r i32 | color u32

v4 drops `tx_offset`, which no parser ever read, for a 17-byte record. That
is the only difference, so an existing v3 artifact can be converted in place
rather than re-extracted — which matters because the extractor reads the
SpacetimeDB commitlog, and the commitlog this artifact was built from is no
longer the live one. The v3 file is the only remaining copy of that history.

Usage: convert-v3-to-v4.py <in.bin> <out.bin>
"""
import sys

V3_MAGIC = b"HEXELHIST\x03"
V4_MAGIC = b"HEXELHIST\x04"
V3_RECORD = 25
V4_RECORD = 17
# Records per read, chosen so the buffer stays a few MB rather than loading a
# quarter-gigabyte artifact into memory at once.
BATCH = 65536


def main(src_path, dst_path):
    with open(src_path, "rb") as src:
        magic = src.read(len(V3_MAGIC))
        if magic == V4_MAGIC:
            sys.exit(f"{src_path} is already v4; nothing to do")
        if magic != V3_MAGIC:
            sys.exit(f"{src_path} is not a recovered-history artifact (magic {magic!r})")

        with open(dst_path, "wb") as dst:
            dst.write(V4_MAGIC)
            converted = 0
            while True:
                chunk = src.read(V3_RECORD * BATCH)
                if not chunk:
                    break
                if len(chunk) % V3_RECORD:
                    sys.exit(f"trailing {len(chunk) % V3_RECORD} bytes: source is truncated")
                out = bytearray()
                for at in range(0, len(chunk), V3_RECORD):
                    record = chunk[at:at + V3_RECORD]
                    # kind, then everything after the dropped u64 offset.
                    out += record[0:1] + record[9:V3_RECORD]
                dst.write(out)
                converted += len(chunk) // V3_RECORD

    print(f"converted {converted:,} records: {src_path} -> {dst_path}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
