#!/usr/bin/env bash
#
# Refresh the recovered tile history from the live replica's commitlog.
#
# Run by the hexel-history.timer systemd unit once a day; safe to run by hand.
# Resolves the replica directory from the running server rather than hardcoding
# an id: republishing a database (or restoring a backup) gives it a new replica
# directory, and pointing the extractor at a stale one silently produces
# history for a database nobody is playing.
#
#   ./refresh.sh              extract, then rebuild the web bundle
#   ./refresh.sh --no-build   extract only (bundle keeps the previous history)
#
# The extractor resumes from a checkpoint, so a daily run only parses
# transactions committed since the last one. The checkpoint is only valid for
# the log it was built from, so a replica change forces a fresh extraction.

set -euo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
DATA_DIR="${SPACETIME_DATA_DIR:-$HOME/.local/share/spacetime/data}"
DB_NAME="${HEXEL_DB:-hexel}"
OUT_DIR="$REPO_ROOT/tools/history-extractor/output"
BIN="$OUT_DIR/hexel-tile-history-v4.bin"
# Log-order working file the extractor appends to (and resumes from). The
# shipped BIN is re-emitted from it in timestamp order every run, so this one
# is the file the checkpoint belongs to.
RAW="$OUT_DIR/hexel-tile-history-v4-raw.bin"
CHECKPOINT="$OUT_DIR/hexel-tile-history-v4.checkpoint"
# Island placement history recovered from the commitlog this database was
# seeded from (see README). Static — that database no longer exists — and
# merged into every emitted BIN, because a bulk import cannot carry it.
LEGACY="$OUT_DIR/hexel-legacy-history.bin"
REPLICA_MARKER="$OUT_DIR/.replica-id"

build_bundle=1
[ "${1:-}" = "--no-build" ] && build_bundle=0

log() { printf '[hexel-history] %s\n' "$*"; }

# Exactly one replica directory is the normal case; if the datastore ever
# holds several, prefer the most recently written commitlog, which is the
# database actually taking traffic.
replica_dir=$(find "$DATA_DIR/replicas" -maxdepth 2 -type d -name clog -printf '%T@ %p\n' 2>/dev/null \
    | sort -rn | head -1 | cut -d' ' -f2-)
if [ -z "$replica_dir" ]; then
    log "no replica commitlog under $DATA_DIR/replicas — is the server initialised?"
    exit 1
fi
replica_id=$(basename "$(dirname "$replica_dir")")
log "database=$DB_NAME replica=$replica_id clog=$replica_dir"

mkdir -p "$OUT_DIR"

# A checkpoint records an offset into one specific commitlog. Resuming it
# against a different replica's log would misparse from the first record, so
# start clean whenever the replica changes.
previous_id=$(cat "$REPLICA_MARKER" 2>/dev/null || true)
if [ "$previous_id" != "$replica_id" ]; then
    # Preserve whatever is there before a fresh extraction overwrites it. An
    # existing history with no marker predates this script and came from some
    # earlier replica — and once a database has been republished, that file
    # can be the only surviving copy of the history in its commitlog, which
    # no longer exists to re-extract from.
    if [ -f "$BIN" ]; then
        stamp=$(date -u +%Y%m%dT%H%M%SZ)
        archived="$BIN.replica-${previous_id:-unknown}.$stamp"
        log "replica changed (${previous_id:-unknown} -> $replica_id); archiving previous history"
        mv "$BIN" "$archived"
        log "previous history kept at $archived"
    fi
    rm -f "$CHECKPOINT" "$RAW"
    printf '%s' "$replica_id" > "$REPLICA_MARKER"
fi
# A checkpoint without its working file (or the other way round) would resume
# from the wrong byte offset and interleave two logs' records.
if [ -f "$CHECKPOINT" ] && [ ! -f "$RAW" ]; then
    log "checkpoint has no working file; extracting from scratch"
    rm -f "$CHECKPOINT"
fi

log "extracting..."
# Prefer --offline so a scheduled run can't be derailed by the network, but
# fall back when the pinned SpacetimeDB git tag isn't in Cargo's cache yet
# (see README: the first build has to fetch it).
# Build offline first so a scheduled run can't be derailed by the network,
# falling back only when the pinned SpacetimeDB git tag isn't in Cargo's cache
# yet (see README: the first build has to fetch it). Building and running are
# separate steps on purpose — folding them into one `cargo run` meant a
# failure inside the extractor itself (a validation error, say) looked like a
# build failure and triggered a pointless network retry that failed the same
# way, with the real message discarded.
if ! cargo build --offline --quiet \
        --manifest-path "$REPO_ROOT/tools/history-extractor/Cargo.toml" 2>/dev/null; then
    log "offline build unavailable; retrying with network access"
    cargo build --quiet --manifest-path "$REPO_ROOT/tools/history-extractor/Cargo.toml"
fi
"$REPO_ROOT/tools/history-extractor/target/debug/history-extractor" \
    "$replica_dir" "$CHECKPOINT" "$RAW" "$BIN" "$LEGACY"
[ -f "$LEGACY" ] || log "no recovered island history at $LEGACY; islands will hold their imported slots"
log "history is $(du -h "$BIN" | cut -f1) at $BIN"

# The title screen's baked map ages the same way the history does: islands
# move slots on every re-rank, so an image baked days ago places them where
# they no longer are, and the intro zoom from image to live world visibly
# jumps. Rebaked here so the two artifacts are always from the same moment.
# Non-fatal: a stale backdrop is worth shipping, a failed refresh is not.
if [ -x "$REPO_ROOT/tools/map-image/render_map.py" ]; then
    log "rebaking the title map..."
    if (cd "$REPO_ROOT/tools/map-image" \
            && ./render_map.py "$DB_NAME" ../../client/assets/title-map >/dev/null); then
        log "title map rebaked"
    else
        log "WARNING: title map rebake failed; keeping the previous image"
    fi
fi

if [ "$build_bundle" -eq 1 ]; then
    # The history is served as a plain sibling of the page and fetched at
    # runtime, so refreshing the .bin alone never reaches a browser — only a
    # build copies it into client/web/ for players to see the newer history.
    log "rebuilding web bundle so the new history ships..."
    (cd "$REPO_ROOT" && ./build-web.sh >/dev/null)
    served="$REPO_ROOT/client/web/hexel-tile-history.bin"
    expected=$(stat -c %s "$BIN")
    if [ "$(stat -c %s "$served" 2>/dev/null || echo 0)" != "$expected" ]; then
        log "ERROR: $served is not the $expected bytes just extracted"
        log "the web build did not copy the history; players would get the previous one"
        exit 1
    fi
    # Caddy serves the precompressed siblings, not this file, to anyone who
    # accepts zstd/gzip — and build-web.sh only rebuilds them when the source
    # is NEWER, so a restored .bin or a clock skew can leave a stale pair
    # behind a build that reports success. Comparing decompressed sizes costs
    # a second and catches a stale sibling as well as a truncated one, which
    # is how a corrupt .gz shipped once (see the ?v= bump in game.html).
    for ext in zst gz; do
        case "$ext" in
            zst) size=$(zstd -dc "$served.$ext" 2>/dev/null | wc -c) || size=0 ;;
            gz)  size=$(gzip -dc "$served.$ext" 2>/dev/null | wc -c) || size=0 ;;
        esac
        if [ "$size" != "$expected" ]; then
            log "ERROR: $served.$ext decompresses to $size bytes, not $expected"
            log "browsers accepting $ext would get a stale or truncated history"
            exit 1
        fi
    done
    log "web bundle rebuilt (history: $expected bytes, .zst/.gz verified)"
else
    log "skipping web rebuild (--no-build); run ./build-web.sh to ship this history"
fi
