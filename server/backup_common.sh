#!/usr/bin/env bash

# Shared helpers for the local SpacetimeDB data-directory backup scripts.
# Both scripts expect the server to be the foreground command in this tmux
# session, so Ctrl-C returns the pane to its shell and a new start command can
# be sent to that same pane.

set -euo pipefail

SPACETIME_TMUX_SESSION="${SPACETIME_TMUX_SESSION:-spacetime}"
SPACETIME_DATA_DIR="${SPACETIME_DATA_DIR:-$HOME/.local/share/spacetime/data}"
SPACETIME_BACKUP_DIR="${SPACETIME_BACKUP_DIR:-$HOME/spacetime-backups}"
SPACETIME_START_COMMAND="${SPACETIME_START_COMMAND:-spacetime start --data-dir $SPACETIME_DATA_DIR}"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

server_is_running() {
    pgrep -f '[s]pacetimedb-standalone' >/dev/null 2>&1
}

require_server() {
    require_command tmux
    require_command zstd
    require_command tar

    tmux has-session -t "$SPACETIME_TMUX_SESSION" 2>/dev/null || \
        die "tmux session '$SPACETIME_TMUX_SESSION' does not exist"
    server_is_running || die "SpacetimeDB is not running in tmux session '$SPACETIME_TMUX_SESSION'"
    [[ -d "$SPACETIME_DATA_DIR" ]] || die "SpacetimeDB data directory does not exist: $SPACETIME_DATA_DIR"
}

stop_server() {
    printf 'Stopping SpacetimeDB in tmux session %s...\n' "$SPACETIME_TMUX_SESSION"
    tmux send-keys -t "$SPACETIME_TMUX_SESSION" C-c

    local deadline=$((SECONDS + 30))
    while server_is_running; do
        (( SECONDS < deadline )) || die 'SpacetimeDB did not stop within 30 seconds'
        sleep 1
    done
}

start_server() {
    printf 'Starting SpacetimeDB in tmux session %s...\n' "$SPACETIME_TMUX_SESSION"
    tmux send-keys -t "$SPACETIME_TMUX_SESSION" -l "$SPACETIME_START_COMMAND"
    tmux send-keys -t "$SPACETIME_TMUX_SESSION" C-m

    local deadline=$((SECONDS + 30))
    until server_is_running; do
        (( SECONDS < deadline )) || die 'SpacetimeDB did not start within 30 seconds'
        sleep 1
    done
}

create_backup() {
    local prefix="$1"
    local stamp archive temporary_archive

    stamp="$(date -u +%Y%m%dT%H%M%SZ)"
    archive="$SPACETIME_BACKUP_DIR/$prefix-$stamp.tar.zst"
    temporary_archive="$archive.tmp.$$"

    mkdir -p "$SPACETIME_BACKUP_DIR"
    tar -C "$SPACETIME_DATA_DIR" --exclude=cache --exclude=spacetime.pid \
        -cf - control-db program-bytes replicas metadata.toml config.toml \
        | zstd -T0 -19 -q -o "$temporary_archive"
    mv "$temporary_archive" "$archive"

    printf 'Backup saved to %s\n' "$archive"
}
