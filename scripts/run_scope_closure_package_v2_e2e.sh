#!/usr/bin/env bash
set -euo pipefail

worker_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
database_root="${WORKER_E2E_DATABASE_ROOT:-}"

if [[ "$database_root" != /* || ! -f "$database_root/supabase/config.toml" ]]; then
  echo "Set WORKER_E2E_DATABASE_ROOT to an absolute isolated Supabase project directory." >&2
  exit 2
fi
if [[ "${WORKER_E2E_ALLOW_RESET:-}" != "isolated" ]]; then
  echo "Set WORKER_E2E_ALLOW_RESET=isolated only after verifying the selected project can be reset." >&2
  exit 2
fi
project_id="$(sed -n 's/^project_id = "\([^"]*\)"/\1/p' "$database_root/supabase/config.toml" | head -n 1)"
if [[ -z "$project_id" || "$project_id" == "database-engine" ]]; then
  echo "Refusing to reset an unverified or shared database project." >&2
  exit 2
fi

(
  cd "$database_root"
  supabase db reset
)

eval "$(cd "$database_root" && supabase status -o env)"
export DATABASE_URL="$DB_URL"
export S3_ENDPOINT="$STORAGE_S3_URL"
export S3_REGION="$S3_PROTOCOL_REGION"
export S3_BUCKET="lca-results-e2e"
export S3_ACCESS_KEY_ID="$S3_PROTOCOL_ACCESS_KEY_ID"
export S3_SECRET_ACCESS_KEY="$S3_PROTOCOL_ACCESS_KEY_SECRET"
export SNAPSHOT_BUILDER_BIN="$worker_root/target/debug/snapshot_builder"
export SNAPSHOT_REPORT_MODE="disabled"
export TIDAS_BIN="${TIDAS_BIN:-tidas}"
export TIDAS_EXPECTED_VERSION="${TIDAS_EXPECTED_VERSION:-0.3.2}"

"$TIDAS_BIN" version --format json --progress never >/dev/null
"$TIDAS_BIN" validate --describe --format json --progress never >/dev/null

cd "$worker_root"
cargo build -p solver-worker --bin snapshot_builder
cargo test -p solver-worker --test scope_closure_package_v2_e2e \
  certified_snapshot_lifecycle_is_frozen_reusable_and_fail_closed -- --exact --ignored --nocapture

# Both scenarios use the public release method identities; a fresh isolated
# database keeps their fixture rows independent without weakening constraints.
(
  cd "$database_root"
  supabase db reset
)
cargo test -p solver-worker --test scope_closure_package_v2_e2e \
  review_submit_source_closure_benchmark_fixture -- --exact --ignored --nocapture
