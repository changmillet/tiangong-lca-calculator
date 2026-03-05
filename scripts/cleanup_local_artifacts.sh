#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

DRY_RUN=0
WITH_TARGET=0

usage() {
  cat <<'USAGE'
Usage:
  scripts/cleanup_local_artifacts.sh [options]

Options:
  --dry-run       show what would be removed, do not delete
  --with-target   also remove Rust build artifacts under ./target
  -h, --help      show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    --with-target)
      WITH_TARGET=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
done

remove_path() {
  local path="$1"
  if [ ! -e "$path" ]; then
    return
  fi

  if [ "$DRY_RUN" -eq 1 ]; then
    echo "[dry-run] rm -rf $path"
    return
  fi

  echo "[cleanup] rm -rf $path"
  rm -rf "$path"
}

echo "[info] repository root: $ROOT_DIR"
remove_path "$ROOT_DIR/logs"
remove_path "$ROOT_DIR/reports"
remove_path "$ROOT_DIR/tools/bw25-validator/.venv"

if [ "$WITH_TARGET" -eq 1 ]; then
  remove_path "$ROOT_DIR/target"
fi

echo "[done] local artifact cleanup finished"

