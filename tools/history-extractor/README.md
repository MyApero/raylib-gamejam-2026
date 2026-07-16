# Hexel local tile-history recovery

`history-extractor` reads a SpacetimeDB replica commitlog without opening the
database for writing. It extracts every `island_cell` and `margin_cell`
mutation in committed transaction order, including the delete that precedes an
overwrite and an erase.

The recovered local artifact is deliberately ignored by Git:

`tools/history-extractor/output/hexel-tile-history.bin`

## Recreate or resume it

```sh
cargo run --offline --quiet --manifest-path tools/history-extractor/Cargo.toml -- \
  "$HOME/.local/share/spacetime/data/replicas/6000001/clog" \
  tools/history-extractor/output/hexel-tile-history-v3.checkpoint \
  tools/history-extractor/output/hexel-tile-history-v3.bin
```

The checkpoint makes a long scan resumable. It is only advanced after the
matching output bytes have been flushed. On resume, any partial bytes beyond
the checkpoint are removed before more records are appended. Note: the
per-owner seed-hue tracking used to resolve v3's border colors (see below)
is NOT part of the checkpoint — a resumed run starts that map empty, so
prefer a from-scratch run when island border colors matter.

## Binary format

The file starts with the ten-byte magic value `HEXELHIST\x03`. Each following
record is 25 bytes, little-endian:

| Bytes | Field |
| --- | --- |
| 1 | operation: `0` island delete, `1` island insert, `2` margin delete, `3` margin insert |
| 8 | SpacetimeDB transaction offset |
| 4 | island id (`u32`; zero for margin records) |
| 4 | axial `q` coordinate (`i32`) |
| 4 | axial `r` coordinate (`i32`) |
| 4 | packed HSV color (`u32`) |

Island placement is recorded too: operation `4` deletes an island and `5`
inserts it. For those records the island id is populated and `q` contains the
island slot; `r` is zero. As of v3, a kind-5 record's `color` carries the
island's border color resolved AT THAT POINT in history — the owner's
`border_color` pin if one was set, else their seed hue (from tracking
`inventory`'s `obtained_with.is_none()` row per owner as the log is walked),
or `0xFFFFFFFF` (never a valid packed HSV value) if neither could be
resolved. Earlier versions left this zero and let replay fall back to a
live-table lookup, which comes up empty for any island since deleted
(`admin_delete_island`/`delete_account` remove the owner's `Inventory` rows
too) — the border silently rendered as flat gray. This lets playback apply
re-ranking, deletions AND border colors at their historical positions.

Rows in a commitlog transaction are encoded as inserts followed by deletes.
The extractor writes each recovered transaction as deletes followed by inserts,
which preserves the actual atomic result of a tile overwrite during replay.
