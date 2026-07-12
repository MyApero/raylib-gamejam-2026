#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

case "${1:-}" in
    "")
        "$SCRIPT_DIR/save_backup.sh"
        ;;
    --no-backup)
        ;;
    *)
        printf 'Usage: %s [--no-backup]\n' "${BASH_SOURCE[0]}" >&2
        exit 2
        ;;
esac

spacetime publish -s local --module-path "$SCRIPT_DIR" --delete-data=never --yes=all hexel
