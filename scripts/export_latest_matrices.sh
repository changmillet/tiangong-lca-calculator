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

PROJECT_DIR="$ROOT_DIR/tools/bw25-validator"

if command -v uv >/dev/null 2>&1; then
  exec uv run --project "$PROJECT_DIR" python -m bw25_validator.export_matrices_cli "$@"
fi

PYTHON_BIN="${PYTHON_BIN:-python3}"
if ! command -v "$PYTHON_BIN" >/dev/null 2>&1; then
  echo "missing python runtime: $PYTHON_BIN" >&2
  exit 1
fi

VENV_DIR="${VENV_DIR:-$ROOT_DIR/.venv/bw25-validator}"
if [ ! -x "$VENV_DIR/bin/python" ]; then
  "$PYTHON_BIN" -m venv "$VENV_DIR"
  "$VENV_DIR/bin/pip" install --upgrade pip
  "$VENV_DIR/bin/pip" install -e "$PROJECT_DIR"
fi

exec "$VENV_DIR/bin/python" -m bw25_validator.export_matrices_cli "$@"
