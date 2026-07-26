# Hexel local tile-history recovery

`history-extractor` reads a SpacetimeDB replica commitlog without opening the
database for writing. It extracts every `island_cell` and `margin_cell`
mutation in committed transaction order, including the delete that precedes an
overwrite and an erase.

The recovered local artifact is deliberately ignored by Git:

`tools/history-extractor/output/hexel-tile-history-v4.bin`

The name carries the format version (see *Binary format* below); `build-web.sh`
reads that exact path, so bumping the format means renaming it in both places.

## Prerequisites

This tool links against SpacetimeDB's own `commitlog`/`datastore` crates to
parse the on-disk log directly, so it needs to be built against the *exact*
commit the local `spacetime` server was built from — not just a compatible
semver. `Cargo.toml` pins those crates to a `git` dependency at tag
`v2.6.1`, which matches the CLI this project currently targets:

```sh
spacetime --version   # Commit: 052c83fe... -> tag v2.6.1
```

If the installed `spacetime` gets upgraded, bump the `tag = "v2.6.1"` value
in `tools/history-extractor/Cargo.toml` (all six `spacetimedb-*` entries) to
match, then rebuild — a mismatched commit can silently misparse the log
format. The first build fetches that tag over git, so it needs network
access and can't use `--offline`; once Cargo has it cached, subsequent
builds/runs can go `--offline` again.

## Recreate or resume it

```sh
cargo run --offline --quiet --manifest-path tools/history-extractor/Cargo.toml -- \
  "$HOME/.local/share/spacetime/data/replicas/1/clog" \
  tools/history-extractor/output/hexel-tile-history-v4.checkpoint \
  tools/history-extractor/output/hexel-tile-history-v4-raw.bin \
  tools/history-extractor/output/hexel-tile-history-v4.bin \
  tools/history-extractor/output/hexel-legacy-history.bin
```

Arguments, in order: the replica's `clog` directory (required), a checkpoint
file path (optional — omit it to run without resumability), a working-file
path (optional — omit it for a dry-run scan that only prints the
`transactions=`/`tile_inserts=`/`tile_deletes=` summary and writes nothing), a
shipped-file path (optional — omit it to skip the sort/emit pass), and a
recovered island-history path (optional, see below).

`refresh.sh` passes all five and is what the daily timer runs; the arguments
above are for driving it by hand.

## Two files, two orders

The working file is written in commitlog order so a checkpoint can append to
it, and carries an extra 8-byte timestamp per record (magic `HEXELRAW\x01`,
33-byte records). The shipped file is re-emitted from it in *chronological*
order, without that timestamp.

They differ because the current database was seeded by a bulk import, which
replayed weeks of painting as a handful of transactions in table order — so
commitlog order put every island cell before every margin cell instead of
interleaving them as they were actually painted. Sorting on the timestamp
each row already carries (`painted_at`, `created_at`) restores the real order.
Events whose own row does not say when they happened — any delete, or an
island re-inserted at a new leaderboard slot, which keeps its original
`created_at` — are stamped with the latest `painted_at` seen so far instead.

## Recovered legacy history

A bulk import carries a world, not a history. Every island arrives at the slot
it held on import day and every cell at its final colour, so a replay of the
imported era shows a frozen leaderboard being painted once, with nothing ever
repainted. `output/hexel-legacy-history.bin` is a working file recovered from
the commitlog of the database this one was seeded from and the emit pass
merges it into every shipped file. Import-era island events for an island that
history knows are re-stamped to the point it ends, so they read as the
catch-up to the imported layout rather than as foundings.

That log holds 19.5M events — 316 MB in the shipped format, against a bundle
where the wasm itself is 5.6 MB. So it is **sampled**: island placement,
margin history and deletes are kept whole (about 51k events between them), and
island-cell paints are kept one in N, with N solved for a byte budget. Every
paint overwrites, so any subset stays consistent, and the final state comes
from the import rather than from this file — sampling can only cost
intermediate detail, never correctness. Raising the budget means re-running
the sampler over the working file; the stride is the only thing that changes.

It is a static artifact — that database no longer exists. It was rebuilt from
a datastore backup by extracting `replicas/<id>/clog` from the tarball and
running this tool over it with no shipped-file argument, then sampling the
working file it produced. Regenerating it means going back to a backup;
nothing in a normal run touches it, and a run without it still works (the
replay just shows the imported world with no history behind it).

The checkpoint makes a long scan resumable. It is only advanced after the
matching output bytes have been flushed. On resume, any partial bytes beyond
the checkpoint are removed before more records are appended. Note: the
per-owner seed-hue tracking used to resolve v3's border colors (see below)
is NOT part of the checkpoint — a resumed run starts that map empty, so
prefer a from-scratch run when island border colors matter.

## Binary format

The shipped file starts with the ten-byte magic value `HEXELHIST\x04`. Each
following record is 17 bytes, little-endian:

| Bytes | Field |
| --- | --- |
| 1 | operation: `0` island delete, `1` island insert, `2` margin delete, `3` margin insert |
| 4 | island id (`u32`; zero for margin records) |
| 4 | axial `q` coordinate (`i32`) |
| 4 | axial `r` coordinate (`i32`) |
| 4 | packed HSV color (`u32`) |

The working file (`HEXELRAW\x01`, 33 bytes) is the same record with the
SpacetimeDB transaction offset in front of the payload and the sort timestamp
after it. v3 shipped that offset too; the client never read it, and at eight
of twenty-five bytes it decided how much history could fit in the bundle.

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

An update reaches the log as a delete plus an insert of the same row, and a
delete whose insert is in the same transaction is dropped: replay applies an
insert by overwriting, so keeping both only made the row blink out and back —
a whole leaderboard re-rank vanished and reappeared, and every repainted tile
flashed empty. It also halves the working file, since almost every delete in
this game's log is half of a repaint.
