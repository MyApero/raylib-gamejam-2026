#!/usr/bin/env python3
"""One-off migration: copy the `hexel` database (restored prod backup whose
owner identity no longer exists locally) into a fresh, locally-owned
database, dropping bot-generated merge events in flight.

Reads old rows through the HTTP SQL endpoint (JSON; no game connection, so
the archive DB is not mutated by client_connected) and writes them through
the temporary db-migrator module's import_* reducers via `spacetime call`.

Two sources, selected by `HEXEL_DUMP_DIR`:
  unset  — read the live `OLD_DB` over HTTP SQL (the original run).
  set    — read `dump.py`'s JSON instead. Used for the second pass, which
           reloads into a database that reclaimed the `hexel` name after the
           control-db wipe; the source database no longer exists by then.
`HEXEL_TARGET_DB` overrides the destination (default `hexel`).

Bot filter: every identity found in ~/.spacetimedb_client_credentials/
hexel-bot-* plus the six bot identities still present in the live user
table, EXCLUDING the heart bot (its events are kept on principle; it has
none in practice). Re-running over an already-filtered dump drops nothing.
"""

import base64
import glob
import json
import os
import re
import subprocess
import sys
import urllib.request

HOST = "http://127.0.0.1:3000"
OLD_DB = "hexel"
NEW_DB = os.environ.get("HEXEL_TARGET_DB", "hexel")
DUMP_DIR = os.environ.get("HEXEL_DUMP_DIR")
BATCH = 2000

HEART_CRED = os.path.expanduser("~/.spacetimedb_client_credentials/hexel-bot-heart")

# Bots whose user rows still exist in the live database (credentials for
# these also exist, but belt and braces: list them explicitly too).
KNOWN_BOT_IDENTITIES = {
    "0xc200b1852f758ad3d3d1b430539a2faf499ccc7c52ae448827caa5db0f661a0e",  # Hexa bot 1
    "0xc200a9e28e4eb6b8639c860737f96a139f49e1531f808f3da03ecc9151a6e8e6",  # Hexa bot 2
    "0xc20039a17e76cc272be6c0b4b4d927be7b9b23be881f4c308e691367411456f8",  # Hexa bot 3
    "0xc2005adb183bfa5dd150382687b9aa3b992a363098547d7841d040a95ff993e1",  # Hexa bot 4
    "0xc20028d7dcce80bbc9d807badfc85a94d9510e26a87bcfd8804552e338c94ff1",  # Hexa bot 5
    "0xc20082c4ea25ec2b0e80e874d60771b53e27345114e13e6e79b7ccc4025791c6",  # Merge with me! (center)
}


def cli_token():
    src = open(os.path.expanduser("~/.config/spacetime/cli.toml")).read()
    return re.search(r'spacetimedb_token = "([^"]+)"', src).group(1)


TOKEN = cli_token()


def sql(query):
    req = urllib.request.Request(
        f"{HOST}/v1/database/{OLD_DB}/sql",
        data=query.encode(),
        headers={"Authorization": f"Bearer {TOKEN}"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        payload = json.load(resp)
    result = payload[0]
    names = [e["name"]["some"] for e in result["schema"]["elements"]]
    types = [e["algebraic_type"] for e in result["schema"]["elements"]]
    return [dict(zip(names, row)) for row in result["rows"]], types


def fetch(table, names):
    """`(rows, types)` for `table`, from the live OLD_DB or dump.py's JSON.

    Both paths return columns in `names` order, which `convert_rows` relies
    on — dump.py selects with these same per-table column lists.
    """
    if not DUMP_DIR:
        return sql(f"SELECT {', '.join(names)} FROM {table}")
    with open(os.path.join(DUMP_DIR, f"{table}.json")) as fh:
        payload = json.load(fh)
    rows = [dict(zip(payload["names"], row)) for row in payload["rows"]]
    return rows, payload["types"]


def is_option(ty):
    return "Sum" in ty and [v["name"]["some"] for v in ty["Sum"]["variants"]] == ["some", "none"]


def convert(value, ty):
    """HTTP-SQL JSON value -> `spacetime call` JSON arg value."""
    if is_option(ty):
        idx, payload = value
        return {"some": convert(payload, ty["Sum"]["variants"][0]["algebraic_type"])} if idx == 0 else None
    return value


def convert_rows(rows, types, names):
    typed = list(zip(names, types))
    out = []
    for row in rows:
        out.append({name: convert(row[name], ty) for name, ty in typed})
    return out


def call(reducer, rows, extra_args=()):
    """Call import reducer in batches; `rows` is the single Vec argument."""
    ok = 0
    for i in range(0, len(rows), BATCH):
        chunk = rows[i : i + BATCH]
        args = [json.dumps(chunk, separators=(",", ":"))] + [json.dumps(a) for a in extra_args]
        proc = subprocess.run(
            ["spacetime", "call", "-s", "local", NEW_DB, reducer, *args],
            capture_output=True,
            text=True,
        )
        if proc.returncode != 0:
            print(f"ERROR in {reducer} batch {i}: {proc.stdout}{proc.stderr}", file=sys.stderr)
            sys.exit(1)
        ok += len(chunk)
        print(f"  {reducer}: {ok}/{len(rows)}")


def jwt_identity(path):
    data = open(path, "rb").read()
    i = data.find(b"eyJ")
    payload = data[i:].decode("ascii", "ignore").split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return "0x" + json.loads(base64.urlsafe_b64decode(payload))["hex_identity"]


def bot_identities():
    bots = set(KNOWN_BOT_IDENTITIES)
    heart = jwt_identity(HEART_CRED)
    for path in glob.glob(os.path.expanduser("~/.spacetimedb_client_credentials/hexel-bot-*")):
        bots.add(jwt_identity(path))
    bots.discard(heart)  # the heart's events stay
    return bots, heart


def count(db, table):
    req = urllib.request.Request(
        f"{HOST}/v1/database/{db}/sql",
        data=f"SELECT COUNT(*) AS n FROM {table}".encode(),
        headers={"Authorization": f"Bearer {TOKEN}"},
        method="POST",
    )
    with urllib.request.urlopen(req) as resp:
        return json.load(resp)[0]["rows"][0][0]


def main():
    bots, heart = bot_identities()
    print(f"bot filter: {len(bots)} identities (heart {heart[:18]}... kept)")

    # -- config (single row; import_config takes scalar args, not a Vec) --
    rows, types = fetch("config", ["frozen", "admin", "next_rerank_at"])
    cfg = convert_rows(rows, types, ["frozen", "admin", "next_rerank_at"])[0]
    proc = subprocess.run(
        [
            "spacetime", "call", "-s", "local", NEW_DB, "import_config",
            json.dumps(cfg["frozen"]), json.dumps(cfg["admin"]), json.dumps(cfg["next_rerank_at"]),
        ],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        sys.exit(f"import_config failed: {proc.stdout}{proc.stderr}")
    print("import_config: ok")

    # -- users (all offline on the new DB) --
    names = ["identity", "name", "online", "cx", "cy", "last_seen", "hue", "sat", "val", "locked", "xp", "paint_tokens", "tokens_at"]
    rows, types = fetch("user", names)
    users = convert_rows(rows, types, names)
    for u in users:
        u["online"] = False
    call("import_users", users)
    print(f"users: {len(users)}")

    # -- islands (explicit ids preserved) --
    names = ["id", "owner", "slot", "likes", "itch_rate_id", "created_at", "border_color", "border_hidden"]
    rows, types = fetch("island", names)
    rows.sort(key=lambda r: r["id"])
    islands = convert_rows(rows, types, names)
    call("import_islands", islands)
    max_island_id = max(i["id"] for i in islands)
    print(f"islands: {len(islands)} (max id {max_island_id})")
    # NOTE: the island auto-inc sequence must be advanced past
    # `max_island_id` out of band afterwards (`UPDATE st_sequence SET
    # allocated = <manifest island_id_seq_allocated> ...` + server restart)
    # — explicit-id inserts don't touch the sequence, and `advance_island_
    # sequence` can't help here because burning ids via dummy inserts
    # collides with the primary keys just migrated.

    # -- inventory (renumbered, ascending original order) --
    names = ["id", "owner", "hue", "obtained_at", "obtained_with", "from_gift"]
    rows, types = fetch("inventory", names)
    rows.sort(key=lambda r: r["id"])
    call("import_inventory", convert_rows(rows, types, names))

    # -- island cells (explicit packed ids) --
    names = ["id", "island_id", "q", "r", "color", "painted_by", "painted_at"]
    rows, types = fetch("island_cell", names)
    call("import_island_cells", convert_rows(rows, types, names))

    # -- margin cells --
    names = ["id", "q", "r", "color", "painted_by", "painted_at"]
    rows, types = fetch("margin_cell", names)
    call("import_margin_cells", convert_rows(rows, types, names))

    # -- likes / clicks (renumbered) --
    names = ["id", "island_id", "liker"]
    rows, types = fetch("island_like", names)
    rows.sort(key=lambda r: r["id"])
    call("import_island_likes", convert_rows(rows, types, names))
    names = ["id", "island_id", "clicker"]
    rows, types = fetch("island_link_click", names)
    rows.sort(key=lambda r: r["id"])
    call("import_island_link_clicks", convert_rows(rows, types, names))

    # -- hexa events (renumbered) --
    names = ["id", "at", "cx", "cy", "member_count"]
    rows, types = fetch("hexa_event", names)
    rows.sort(key=lambda r: r["id"])
    call("import_hexa_events", convert_rows(rows, types, names))

    # -- merge events: bot-filtered, renumbered --
    names = ["id", "at", "a", "b", "hue_a", "hue_b", "merged_hue", "merged_sat", "merged_val"]
    rows, types = fetch("merge_event", names)
    rows.sort(key=lambda r: r["id"])
    kept = [r for r in rows if r["a"][0] not in bots and r["b"][0] not in bots]
    print(f"merge events: {len(rows)} read, {len(kept)} kept ({len(rows) - len(kept)} bot rows dropped)")
    call("import_merge_events", convert_rows(kept, types, names))

    # -- verification --
    # Source counts come from the dump manifest when reloading, since the
    # source database is gone by then; merge_event is expected to differ
    # from source on a live run (bot rows dropped) but not on a reload.
    tables = ["user", "island", "inventory", "island_cell", "margin_cell",
              "island_like", "island_link_click", "hexa_event", "merge_event"]
    if DUMP_DIR:
        with open(os.path.join(DUMP_DIR, "manifest.json")) as fh:
            source = json.load(fh)["tables"]
    else:
        source = {t: count(OLD_DB, t) for t in tables}

    print("\nverification (source -> new):")
    mismatched = []
    for table in tables:
        got = count(NEW_DB, table)
        expected = source[table]
        flag = ""
        if got != expected:
            # A live run legitimately drops bot merge_events; anything else
            # differing means rows were lost.
            if not (table == "merge_event" and not DUMP_DIR):
                mismatched.append(table)
                flag = "  <-- MISMATCH"
        print(f"  {table}: {expected} -> {got}{flag}")
    if mismatched:
        sys.exit(f"\nrow-count mismatch in: {', '.join(mismatched)}")
    print("\nall table counts match")


if __name__ == "__main__":
    main()
