#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if [ -f .env ]; then
  set -a
  # shellcheck source=/dev/null
  source .env
  set +a
fi

PYTHON_BIN="${PYTHON_BIN:-python}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
  echo "missing python runtime: $PYTHON_BIN" >&2
  exit 1
fi

exec "$PYTHON_BIN" "$ROOT_DIR/scripts/export_unmatched_no_provider_diagnostics.py" "$@"
