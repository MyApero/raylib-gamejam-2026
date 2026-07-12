#!/usr/bin/env bash

# Restore a filesystem backup made by save_backup.sh. A safety backup of the
# current state is made first, and the server is restarted in its tmux pane.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=backup_common.sh
source "$SCRIPT_DIR/backup_common.sh"

usage() {
    printf 'Usage: %s <backup.tar.zst>\n' "${0##*/}" >&2
    exit 2
}

[[ $# -eq 1 ]] || usage
BACKUP_ARCHIVE="$1"
[[ -f "$BACKUP_ARCHIVE" ]] || die "backup archive does not exist: $BACKUP_ARCHIVE"

require_command mktemp
BACKUP_LIST="$(mktemp)"
cleanup_backup_list() {
    rm -f "$BACKUP_LIST"
}
trap cleanup_backup_list EXIT

# Validate the archive before stopping the live server or removing any data.
zstd -t -- "$BACKUP_ARCHIVE" >/dev/null 2>&1
zstd -dc -- "$BACKUP_ARCHIVE" | tar -tf - > "$BACKUP_LIST"
for required in control-db/ program-bytes/ replicas/ metadata.toml config.toml; do
    grep -Fxq "$required" "$BACKUP_LIST" || die "backup archive is missing required entry: $required"
done
if grep -Eq '(^/|(^|/)\.\.(/|$))' "$BACKUP_LIST"; then
    die 'backup archive contains an unsafe path'
fi

server_stopped=0
restart_if_needed() {
    local status=$?
    trap - EXIT
    if (( server_stopped )); then
        printf 'Restarting SpacetimeDB after interrupted restore...\n' >&2
        start_server || true
    fi
    cleanup_backup_list
    exit "$status"
}
trap restart_if_needed EXIT

require_server
stop_server
server_stopped=1
create_backup pre-restore-safety

printf 'Restoring %s...\n' "$BACKUP_ARCHIVE"
rm -rf "$SPACETIME_DATA_DIR/control-db" \
    "$SPACETIME_DATA_DIR/program-bytes" \
    "$SPACETIME_DATA_DIR/replicas" \
    "$SPACETIME_DATA_DIR/metadata.toml" \
    "$SPACETIME_DATA_DIR/config.toml"
zstd -dc -- "$BACKUP_ARCHIVE" | tar -C "$SPACETIME_DATA_DIR" -xf -

start_server
server_stopped=0
