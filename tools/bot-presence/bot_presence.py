#!/usr/bin/env python3
"""Run the ambient bots only while a real player is online.

The bots exist so the board never looks empty, which means they have no job
at all when nobody is there to see it. This stops their systemd units while
the world is empty and starts them again as soon as a human connects.

The check has to live outside the bot: a disconnected client cannot watch the
`user` table for someone arriving, so a self-managing bot would have to
reconnect every few seconds to look, and that flicker is exactly what players
would see. Polling the database from here keeps the bot fully stopped instead.

Reads the same HTTP SQL endpoint as tools/db-migrator, and identifies bots the
way migrate.py does — by the identity inside each persisted credential file —
rather than by display name, which `heart` and `center` share.

    ./bot_presence.py                # manage hexel-bot@heart.service
    ./bot_presence.py heart center   # manage several
    ./bot_presence.py --dry-run      # report only, touch nothing

Run from a systemd timer; see WORK.md. Safe to run by hand.
"""

import base64
import glob
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request

HOST = os.environ.get("HEXEL_HOST", "http://127.0.0.1:3000")
DB = os.environ.get("HEXEL_DB", "hexel")
UNIT = "hexel-bot@{}.service"
CREDENTIALS = "~/.spacetimedb_client_credentials/hexel-bot-*"


def cli_token():
    src = open(os.path.expanduser("~/.config/spacetime/cli.toml")).read()
    return re.search(r'spacetimedb_token = "([^"]+)"', src).group(1)


def jwt_identity(path):
    """Hex identity out of a persisted credential file — mirrors migrate.py."""
    data = open(path, "rb").read()
    i = data.find(b"eyJ")
    payload = data[i:].decode("ascii", "ignore").split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return "0x" + json.loads(base64.urlsafe_b64decode(payload))["hex_identity"]


def bot_identities():
    found = set()
    for path in glob.glob(os.path.expanduser(CREDENTIALS)):
        try:
            found.add(jwt_identity(path))
        except (OSError, ValueError, KeyError, IndexError):
            # A credential we cannot read just means one identity we will
            # count as a player, which errs toward leaving the bots running.
            print(f"warning: unreadable credential {path}", file=sys.stderr)
    return found


def unwrap(value):
    """An Identity is a single-field product, so HTTP SQL returns it wrapped:
    `[["0x..."]]` rather than the bare hex string `jwt_identity` produces."""
    while isinstance(value, list) and len(value) == 1:
        value = value[0]
    return value


def online_identities():
    request = urllib.request.Request(
        f"{HOST}/v1/database/{DB}/sql",
        data=b"SELECT identity FROM user WHERE online = true",
        headers={"Authorization": f"Bearer {cli_token()}"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        payload = json.load(response)
    return {unwrap(row[0]) for row in payload[0]["rows"]}


def unit_active(unit):
    return subprocess.run(
        ["systemctl", "is-active", "--quiet", unit], check=False
    ).returncode == 0


def set_unit(unit, should_run, dry_run):
    if unit_active(unit) == should_run:
        return
    action = "start" if should_run else "stop"
    if dry_run:
        print(f"would {action} {unit}")
        return
    result = subprocess.run(
        ["systemctl", action, unit], check=False, capture_output=True, text=True
    )
    if result.returncode != 0:
        sys.exit(f"systemctl {action} {unit} failed: {result.stderr.strip()}")
    print(f"[hexel-bots] {action}ed {unit}")


def main():
    args = [a for a in sys.argv[1:] if a != "--dry-run"]
    dry_run = "--dry-run" in sys.argv[1:]
    shapes = args or ["heart"]

    try:
        online = online_identities()
    except (urllib.error.URLError, OSError, ValueError, KeyError, IndexError) as error:
        # Server down or unreachable. Leave the units exactly as they are:
        # acting on a failed read would stop the bots for the whole outage
        # and then leave them stopped, same reasoning as rebake_on_rerank.sh.
        print(f"database unreachable, leaving units alone: {error}", file=sys.stderr)
        return 0

    bots = bot_identities()
    players = online - bots
    for shape in shapes:
        set_unit(UNIT.format(shape), bool(players), dry_run)
    if dry_run:
        print(f"{len(players)} player(s) online, {len(online & bots)} bot(s) connected")
    return 0


if __name__ == "__main__":
    sys.exit(main())
