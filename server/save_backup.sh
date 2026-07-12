#!/usr/bin/env bash

# Create a consistent filesystem backup of the local SpacetimeDB instance.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=backup_common.sh
source "$SCRIPT_DIR/backup_common.sh"

server_stopped=0

restart_if_needed() {
    local status=$?
    trap - EXIT
    if (( server_stopped )); then
        printf 'Restarting SpacetimeDB after interrupted backup...\n' >&2
        start_server || true
    fi
    exit "$status"
}
trap restart_if_needed EXIT

require_server
stop_server
server_stopped=1
create_backup hexel
start_server
server_stopped=0
