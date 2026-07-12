#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

"$SCRIPT_DIR/save_backup.sh"
spacetime publish -s local --module-path "$SCRIPT_DIR" --delete-data=never --yes=all hexel
