#!/usr/bin/env python3
"""Dump every table `migrate.py` migrates to JSON on disk.

Needed because the `hexel` name is bound to the original database, whose
owner identity is lost — it can be neither renamed nor deleted through the
CLI. Freeing the name means wiping the local control-db, which takes the
migrated `hexel2` down with it, so its contents have to survive the wipe
somewhere outside SpacetimeDB. `migrate.py` reads this dump back with
`HEXEL_DUMP_DIR=<dir>`.

Stores the raw HTTP-SQL response shape (column names, algebraic types, rows)
verbatim, so the reload path shares `migrate.py`'s existing type conversion
rather than reimplementing it.

    ./dump.py hexel2 dump/
"""

import json
import os
import re
import sys
import urllib.request

HOST = "http://127.0.0.1:3000"

# Column lists must match migrate.py's, table for table — the reload path
# feeds them straight into the same import_* reducers.
TABLES = {
    "config": ["frozen", "admin", "next_rerank_at"],
    "user": ["identity", "name", "online", "cx", "cy", "last_seen", "hue",
             "sat", "val", "locked", "xp", "paint_tokens", "tokens_at"],
    "island": ["id", "owner", "slot", "likes", "itch_rate_id", "created_at",
               "border_color", "border_hidden"],
    "inventory": ["id", "owner", "hue", "obtained_at", "obtained_with", "from_gift"],
    "island_cell": ["id", "island_id", "q", "r", "color", "painted_by", "painted_at"],
    "margin_cell": ["id", "q", "r", "color", "painted_by", "painted_at"],
    "island_like": ["id", "island_id", "liker"],
    "island_link_click": ["id", "island_id", "clicker"],
    "hexa_event": ["id", "at", "cx", "cy", "member_count"],
    "merge_event": ["id", "at", "a", "b", "hue_a", "hue_b", "merged_hue",
                    "merged_sat", "merged_val"],
}


def cli_token():
    src = open(os.path.expanduser("~/.config/spacetime/cli.toml")).read()
    return re.search(r'spacetimedb_token = "([^"]+)"', src).group(1)


TOKEN = cli_token()


def query(db, sql):
    req = urllib.request.Request(
        f"{HOST}/v1/database/{db}/sql",
        data=sql.encode(),
        headers={"Authorization": f"Bearer {TOKEN}"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)[0]


def main():
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <source-db> <dump-dir>")
    db, out_dir = sys.argv[1], sys.argv[2]
    os.makedirs(out_dir, exist_ok=True)

    manifest = {"source_db": db, "tables": {}}
    for table, names in TABLES.items():
        result = query(db, f"SELECT {', '.join(names)} FROM {table}")
        payload = {
            "names": [e["name"]["some"] for e in result["schema"]["elements"]],
            "types": [e["algebraic_type"] for e in result["schema"]["elements"]],
            "rows": result["rows"],
        }
        # Cross-check against a COUNT(*) so a silently truncated read can't
        # pass for a complete dump — this file is the only copy after the wipe.
        expected = query(db, f"SELECT COUNT(*) AS n FROM {table}")["rows"][0][0]
        if len(payload["rows"]) != expected:
            sys.exit(f"{table}: dumped {len(payload['rows'])} rows but COUNT(*) says {expected}")
        with open(os.path.join(out_dir, f"{table}.json"), "w") as fh:
            json.dump(payload, fh, separators=(",", ":"))
        manifest["tables"][table] = len(payload["rows"])
        print(f"  {table}: {len(payload['rows'])}")

    # The island sequence is the one auto-inc value the reload must restore:
    # island ids are migrated explicitly (island_cell/island_like/
    # island_link_click reference them), so the sequence has to be pushed
    # past the highest id by hand or the next real insert collides.
    seq = query(db, "SELECT sequence_name, allocated FROM st_sequence")
    manifest["island_id_seq_allocated"] = next(
        row[1] for row in seq["rows"] if row[0] == "island_id_seq"
    )
    with open(os.path.join(out_dir, "manifest.json"), "w") as fh:
        json.dump(manifest, fh, indent=2)
    print(f"\nisland_id_seq allocated: {manifest['island_id_seq_allocated']}")
    print(f"dump written to {out_dir}")


if __name__ == "__main__":
    main()
