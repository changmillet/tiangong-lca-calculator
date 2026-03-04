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

SNAPSHOT_ID="${SNAPSHOT_ID:-}"
QUEUE_NAME="${PGMQ_QUEUE:-lca_jobs}"
LOG_DIR="${LOG_DIR:-logs/full-run}"
REPORT_DIR="${REPORT_DIR:-reports/full-run}"
TIMEOUT_SEC="${TIMEOUT_SEC:-1800}"
POLL_SEC="${POLL_SEC:-2}"
DEMAND_PROCESS_IDX="${DEMAND_PROCESS_IDX:-0}"
PRINT_LEVEL="${PRINT_LEVEL:-0.0}"
KEEP_WORKER_ALIVE="${KEEP_WORKER_ALIVE:-0}"

usage() {
  cat <<'USAGE'
Usage:
  scripts/run_full_compute_debug.sh [options]

Options:
  --snapshot-id <uuid>       snapshot id to run (default: latest ready artifact snapshot, then latest snapshot)
  --queue <name>             pgmq queue name (default: lca_jobs)
  --log-dir <dir>            log output directory (default: logs/full-run)
  --report-dir <dir>         report output directory (default: reports/full-run)
  --timeout-sec <sec>        job wait timeout seconds (default: 1800)
  --poll-sec <sec>           job polling interval seconds (default: 2)
  --demand-process-idx <n>   rhs unit demand index (0-based, default: 0)
  --print-level <float>      UMFPACK print level (default: 0.0)
  --keep-worker-alive        do not stop worker when script exits
  -h, --help                 show this help
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --snapshot-id)
      SNAPSHOT_ID="$2"
      shift 2
      ;;
    --queue)
      QUEUE_NAME="$2"
      shift 2
      ;;
    --log-dir)
      LOG_DIR="$2"
      shift 2
      ;;
    --report-dir)
      REPORT_DIR="$2"
      shift 2
      ;;
    --timeout-sec)
      TIMEOUT_SEC="$2"
      shift 2
      ;;
    --poll-sec)
      POLL_SEC="$2"
      shift 2
      ;;
    --demand-process-idx)
      DEMAND_PROCESS_IDX="$2"
      shift 2
      ;;
    --print-level)
      PRINT_LEVEL="$2"
      shift 2
      ;;
    --keep-worker-alive)
      KEEP_WORKER_ALIVE=1
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

if ! [[ "$TIMEOUT_SEC" =~ ^[0-9]+$ ]]; then
  echo "TIMEOUT_SEC must be an integer: $TIMEOUT_SEC" >&2
  exit 2
fi

if ! [[ "$POLL_SEC" =~ ^[0-9]+$ ]]; then
  echo "POLL_SEC must be an integer: $POLL_SEC" >&2
  exit 2
fi

if ! [[ "$DEMAND_PROCESS_IDX" =~ ^[0-9]+$ ]]; then
  echo "DEMAND_PROCESS_IDX must be a non-negative integer: $DEMAND_PROCESS_IDX" >&2
  exit 2
fi

if ! command -v psql >/dev/null 2>&1; then
  echo "missing required command: psql" >&2
  exit 1
fi

CARGO_BIN="${CARGO_BIN:-}"
if [ -z "$CARGO_BIN" ]; then
  if command -v cargo >/dev/null 2>&1; then
    CARGO_BIN="$(command -v cargo)"
  elif [ -x "$HOME/.cargo/bin/cargo" ]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
  else
    echo "missing required command: cargo" >&2
    exit 1
  fi
fi

DB_URL="${DATABASE_URL:-${CONN:-}}"
if [ -z "$DB_URL" ]; then
  echo "missing DB connection: set DATABASE_URL or CONN" >&2
  exit 1
fi

sql_scalar() {
  psql "$DB_URL" -Atq -v ON_ERROR_STOP=1 -c "$1"
}

sql_scalar_soft() {
  psql "$DB_URL" -Atq -c "$1" 2>/dev/null || true
}

sql_exec() {
  psql "$DB_URL" -v ON_ERROR_STOP=1 -c "$1"
}

gen_uuid() {
  cat /proc/sys/kernel/random/uuid
}

if [ -z "$SNAPSHOT_ID" ]; then
  SNAPSHOT_ID="$(sql_scalar_soft "SELECT snapshot_id::text FROM public.lca_snapshot_artifacts WHERE status = 'ready' ORDER BY created_at DESC LIMIT 1;")"
fi

if [ -z "$SNAPSHOT_ID" ]; then
  SNAPSHOT_ID="$(sql_scalar_soft "SELECT id::text FROM public.lca_network_snapshots ORDER BY created_at DESC LIMIT 1;")"
fi

if [ -z "$SNAPSHOT_ID" ]; then
  echo "no snapshot found; run scripts/build_snapshot_from_ilcd.sh first" >&2
  exit 1
fi

MATRIX_SOURCE=""
ARTIFACT_COUNTS="$(sql_scalar_soft "
SELECT
  process_count::text || '|' ||
  flow_count::text || '|' ||
  a_nnz::text || '|' ||
  b_nnz::text || '|' ||
  c_nnz::text
FROM public.lca_snapshot_artifacts
WHERE snapshot_id = '$SNAPSHOT_ID'::uuid
  AND status = 'ready'
ORDER BY created_at DESC
LIMIT 1;
")"

if [ -n "$ARTIFACT_COUNTS" ]; then
  IFS='|' read -r PROCESS_COUNT FLOW_COUNT A_NNZ B_NNZ C_NNZ <<< "$ARTIFACT_COUNTS"
  MATRIX_SOURCE="artifact_metadata"
else
  PROCESS_COUNT="$(sql_scalar_soft "SELECT COUNT(*)::int FROM public.lca_process_index WHERE snapshot_id = '$SNAPSHOT_ID'::uuid;")"
  FLOW_COUNT="$(sql_scalar_soft "SELECT COUNT(*)::int FROM public.lca_flow_index WHERE snapshot_id = '$SNAPSHOT_ID'::uuid;")"
  A_NNZ="$(sql_scalar_soft "SELECT COUNT(*)::bigint FROM public.lca_technosphere_entries WHERE snapshot_id = '$SNAPSHOT_ID'::uuid;")"
  B_NNZ="$(sql_scalar_soft "SELECT COUNT(*)::bigint FROM public.lca_biosphere_entries WHERE snapshot_id = '$SNAPSHOT_ID'::uuid;")"
  C_NNZ="$(sql_scalar_soft "SELECT COUNT(*)::bigint FROM public.lca_characterization_factors WHERE snapshot_id = '$SNAPSHOT_ID'::uuid;")"
  MATRIX_SOURCE="legacy_tables"
fi

PROCESS_COUNT="${PROCESS_COUNT:-0}"
FLOW_COUNT="${FLOW_COUNT:-0}"
A_NNZ="${A_NNZ:-0}"
B_NNZ="${B_NNZ:-0}"
C_NNZ="${C_NNZ:-0}"

if [ "${PROCESS_COUNT:-0}" -le 0 ]; then
  echo "snapshot $SNAPSHOT_ID has zero processes; cannot run solve" >&2
  exit 1
fi

if [ "${A_NNZ:-0}" -le 0 ]; then
  echo "[warn] snapshot $SNAPSHOT_ID has zero technosphere entries; M defaults to identity (I - 0)" >&2
fi

if [ "$DEMAND_PROCESS_IDX" -ge "$PROCESS_COUNT" ]; then
  echo "demand process index out of range: $DEMAND_PROCESS_IDX (process_count=$PROCESS_COUNT)" >&2
  exit 1
fi

mkdir -p "$LOG_DIR"
mkdir -p "$REPORT_DIR"
RUN_TS="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_LOG="$LOG_DIR/run-$RUN_TS.log"
WORKER_LOG="$LOG_DIR/worker-$RUN_TS.log"
REPORT_JSON="$REPORT_DIR/run-$RUN_TS.json"
REPORT_MD="$REPORT_DIR/run-$RUN_TS.md"

RUN_START_EPOCH="$(date +%s)"
RUN_START_ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
WORKER_START_EPOCH=""
WORKER_READY_EPOCH=""
PREPARE_ENQUEUE_EPOCH=""
PREPARE_DONE_EPOCH=""
SOLVE_ENQUEUE_EPOCH=""
SOLVE_DONE_EPOCH=""
PREPARE_JOB_ID=""
SOLVE_JOB_ID=""
RESULT_ID=""
RESULT_ARTIFACT_FORMAT=""
RESULT_ARTIFACT_SIZE=""
RESULT_ARTIFACT_URL=""
RESULT_HAS_INLINE_PAYLOAD=""

exec > >(tee -a "$RUN_LOG") 2>&1

echo "[info] run timestamp: $RUN_TS"
echo "[info] snapshot_id: $SNAPSHOT_ID"
echo "[info] queue_name: $QUEUE_NAME"
echo "[info] matrix_source=$MATRIX_SOURCE"
echo "[info] process_count=$PROCESS_COUNT flow_count=$FLOW_COUNT a_nnz=$A_NNZ b_nnz=$B_NNZ c_nnz=$C_NNZ"
echo "[info] logs:"
echo "  run_log=$RUN_LOG"
echo "  worker_log=$WORKER_LOG"
echo "  report_json=$REPORT_JSON"
echo "  report_md=$REPORT_MD"

WORKER_PID=""
WORKER_BIN="$ROOT_DIR/target/release/solver-worker"

as_json_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

elapsed_or_null() {
  local start="$1"
  local end="$2"
  if [ -n "$start" ] && [ -n "$end" ]; then
    printf '%s' "$((end - start))"
  else
    printf 'null'
  fi
}

write_run_report() {
  local exit_code="$1"
  local run_end_epoch run_end_iso run_status total_elapsed
  local worker_start_elapsed prepare_elapsed solve_elapsed
  local prepare_status solve_status

  run_end_epoch="$(date +%s)"
  run_end_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  total_elapsed="$((run_end_epoch - RUN_START_EPOCH))"
  worker_start_elapsed="$(elapsed_or_null "$WORKER_START_EPOCH" "$WORKER_READY_EPOCH")"
  prepare_elapsed="$(elapsed_or_null "$PREPARE_ENQUEUE_EPOCH" "$PREPARE_DONE_EPOCH")"
  solve_elapsed="$(elapsed_or_null "$SOLVE_ENQUEUE_EPOCH" "$SOLVE_DONE_EPOCH")"

  if [ "$exit_code" -eq 0 ]; then
    run_status="completed"
  else
    run_status="failed"
  fi

  prepare_status="not_started"
  solve_status="not_started"
  if [ -n "$PREPARE_JOB_ID" ]; then
    prepare_status="$(sql_scalar "SELECT COALESCE(status, 'missing') FROM public.lca_jobs WHERE id = '$PREPARE_JOB_ID'::uuid;" 2>/dev/null || echo unknown)"
  fi
  if [ -n "$SOLVE_JOB_ID" ]; then
    solve_status="$(sql_scalar "SELECT COALESCE(status, 'missing') FROM public.lca_jobs WHERE id = '$SOLVE_JOB_ID'::uuid;" 2>/dev/null || echo unknown)"
  fi

  cat > "$REPORT_JSON" <<JSON
{
  "run_ts": $(as_json_string "$RUN_TS"),
  "run_start_utc": $(as_json_string "$RUN_START_ISO"),
  "run_end_utc": $(as_json_string "$run_end_iso"),
  "status": $(as_json_string "$run_status"),
  "exit_code": $exit_code,
  "snapshot_id": $(as_json_string "$SNAPSHOT_ID"),
  "queue_name": $(as_json_string "$QUEUE_NAME"),
  "demand_process_idx": $DEMAND_PROCESS_IDX,
  "print_level": $PRINT_LEVEL,
  "matrix": {
    "source": $(as_json_string "$MATRIX_SOURCE"),
    "process_count": $PROCESS_COUNT,
    "flow_count": $FLOW_COUNT,
    "a_nnz": $A_NNZ,
    "b_nnz": $B_NNZ,
    "c_nnz": $C_NNZ
  },
  "timing_sec": {
    "total": $total_elapsed,
    "worker_startup": $worker_start_elapsed,
    "prepare_job": $prepare_elapsed,
    "solve_job": $solve_elapsed
  },
  "jobs": {
    "prepare_job_id": $(as_json_string "$PREPARE_JOB_ID"),
    "prepare_status": $(as_json_string "$prepare_status"),
    "solve_job_id": $(as_json_string "$SOLVE_JOB_ID"),
    "solve_status": $(as_json_string "$solve_status")
  },
  "result": {
    "id": $(as_json_string "$RESULT_ID"),
    "artifact_format": $(as_json_string "$RESULT_ARTIFACT_FORMAT"),
    "artifact_byte_size": $(if [ -n "$RESULT_ARTIFACT_SIZE" ]; then printf '%s' "$RESULT_ARTIFACT_SIZE"; else printf 'null'; fi),
    "artifact_url": $(if [ -n "$RESULT_ARTIFACT_URL" ]; then as_json_string "$RESULT_ARTIFACT_URL"; else printf 'null'; fi),
    "has_inline_payload": $(if [ -n "$RESULT_HAS_INLINE_PAYLOAD" ]; then printf '%s' "$RESULT_HAS_INLINE_PAYLOAD"; else printf 'null'; fi)
  },
  "paths": {
    "run_log": $(as_json_string "$RUN_LOG"),
    "worker_log": $(as_json_string "$WORKER_LOG")
  }
}
JSON

  cat > "$REPORT_MD" <<MD
# Full Compute Run Report

- run_ts: \`$RUN_TS\`
- status: \`$run_status\` (exit_code=\`$exit_code\`)
- snapshot_id: \`$SNAPSHOT_ID\`
- queue_name: \`$QUEUE_NAME\`
- demand_process_idx: \`$DEMAND_PROCESS_IDX\`

## Timing (sec)

- total: \`$total_elapsed\`
- worker_startup: \`$worker_start_elapsed\`
- prepare_job: \`$prepare_elapsed\`
- solve_job: \`$solve_elapsed\`

## Matrix

- source: \`$MATRIX_SOURCE\`
- process_count: \`$PROCESS_COUNT\`
- flow_count: \`$FLOW_COUNT\`
- a_nnz: \`$A_NNZ\`
- b_nnz: \`$B_NNZ\`
- c_nnz: \`$C_NNZ\`

## Jobs

- prepare_job_id: \`$PREPARE_JOB_ID\`
- prepare_status: \`$prepare_status\`
- solve_job_id: \`$SOLVE_JOB_ID\`
- solve_status: \`$solve_status\`

## Result

- id: \`$RESULT_ID\`
- artifact_format: \`$RESULT_ARTIFACT_FORMAT\`
- artifact_byte_size: \`$RESULT_ARTIFACT_SIZE\`
- artifact_url: \`$RESULT_ARTIFACT_URL\`
- has_inline_payload: \`$RESULT_HAS_INLINE_PAYLOAD\`

## Logs

- run_log: \`$RUN_LOG\`
- worker_log: \`$WORKER_LOG\`
MD
}

cleanup() {
  if [ "$KEEP_WORKER_ALIVE" = "1" ]; then
    return
  fi

  if [ -n "$WORKER_PID" ] && kill -0 "$WORKER_PID" >/dev/null 2>&1; then
    echo "[info] stopping worker pid=$WORKER_PID"
    kill "$WORKER_PID" >/dev/null 2>&1 || true
    wait "$WORKER_PID" >/dev/null 2>&1 || true
  fi
}

on_exit() {
  local status="$1"
  set +e
  write_run_report "$status"
  cleanup
}
trap 'on_exit $?' EXIT

if [ ! -x "$WORKER_BIN" ]; then
  echo "[info] building release binary: $WORKER_BIN"
  "$CARGO_BIN" build -p solver-worker --release >>"$WORKER_LOG" 2>&1
fi

echo "[info] starting solver-worker in queue mode"
WORKER_START_EPOCH="$(date +%s)"
(
  export DATABASE_URL="$DB_URL"
  export PGMQ_QUEUE="$QUEUE_NAME"
  export SOLVER_MODE=worker
  export RUST_LOG="${RUST_LOG:-info,solver_worker=debug,solver_core=debug}"
  export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"
  "$WORKER_BIN"
) >>"$WORKER_LOG" 2>&1 &
WORKER_PID="$!"

sleep 3
if ! kill -0 "$WORKER_PID" >/dev/null 2>&1; then
  echo "[error] worker failed to start, tailing log:" >&2
  tail -n 120 "$WORKER_LOG" >&2 || true
  exit 1
fi
WORKER_READY_EPOCH="$(date +%s)"
echo "[info] worker pid=$WORKER_PID"

enqueue_prepare() {
  local job_id="$1"
  sql_exec "
  WITH payload AS (
    SELECT jsonb_build_object(
      'type', 'prepare_factorization',
      'job_id', '$job_id'::uuid,
      'snapshot_id', '$SNAPSHOT_ID'::uuid,
      'print_level', $PRINT_LEVEL::double precision
    ) AS message
  ),
  ins AS (
    INSERT INTO public.lca_jobs (
      id, job_type, snapshot_id, status, payload, created_at, updated_at
    )
    SELECT
      '$job_id'::uuid,
      'prepare_factorization',
      '$SNAPSHOT_ID'::uuid,
      'queued',
      message,
      NOW(),
      NOW()
    FROM payload
    RETURNING payload
  )
  SELECT pgmq.send('$QUEUE_NAME', payload) AS msg_id
  FROM ins;
  "
}

enqueue_solve() {
  local job_id="$1"
  sql_exec "
  WITH rhs AS (
    SELECT to_jsonb(
      array_agg(
        CASE WHEN idx = $DEMAND_PROCESS_IDX THEN 1.0::double precision ELSE 0.0::double precision END
        ORDER BY idx
      )
    ) AS vec
    FROM generate_series(0, $PROCESS_COUNT - 1) AS idx
  ),
  payload AS (
    SELECT jsonb_build_object(
      'type', 'solve_one',
      'job_id', '$job_id'::uuid,
      'snapshot_id', '$SNAPSHOT_ID'::uuid,
      'rhs', (SELECT vec FROM rhs),
      'solve', jsonb_build_object(
        'return_x', true,
        'return_g', true,
        'return_h', true
      ),
      'print_level', $PRINT_LEVEL::double precision
    ) AS message
  ),
  ins AS (
    INSERT INTO public.lca_jobs (
      id, job_type, snapshot_id, status, payload, created_at, updated_at
    )
    SELECT
      '$job_id'::uuid,
      'solve_one',
      '$SNAPSHOT_ID'::uuid,
      'queued',
      message,
      NOW(),
      NOW()
    FROM payload
    RETURNING payload
  )
  SELECT pgmq.send('$QUEUE_NAME', payload) AS msg_id
  FROM ins;
  "
}

poll_job() {
  local job_id="$1"
  local label="$2"
  local deadline=$(( $(date +%s) + TIMEOUT_SEC ))

  while true; do
    local status
    status="$(sql_scalar "SELECT COALESCE(status, 'missing') FROM public.lca_jobs WHERE id = '$job_id'::uuid;")"
    local updated_at
    updated_at="$(sql_scalar "SELECT COALESCE(to_char(updated_at, 'YYYY-MM-DD HH24:MI:SS'), 'n/a') FROM public.lca_jobs WHERE id = '$job_id'::uuid;")"

    echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $label status=$status updated_at=$updated_at"

    case "$status" in
      completed|ready)
        sql_exec "SELECT jsonb_pretty(COALESCE(diagnostics, '{}'::jsonb)) AS diagnostics FROM public.lca_jobs WHERE id = '$job_id'::uuid;"
        return 0
        ;;
      failed)
        sql_exec "SELECT jsonb_pretty(COALESCE(diagnostics, '{}'::jsonb)) AS diagnostics FROM public.lca_jobs WHERE id = '$job_id'::uuid;"
        return 1
        ;;
      missing)
        echo "[error] job not found: $job_id" >&2
        return 1
        ;;
      *)
        ;;
    esac

    if [ "$(date +%s)" -gt "$deadline" ]; then
      echo "[error] timeout waiting for $label (job_id=$job_id, timeout_sec=$TIMEOUT_SEC)" >&2
      return 1
    fi
    sleep "$POLL_SEC"
  done
}

PREPARE_JOB_ID="$(gen_uuid)"
SOLVE_JOB_ID="$(gen_uuid)"

echo "[info] enqueue prepare job: $PREPARE_JOB_ID"
PREPARE_ENQUEUE_EPOCH="$(date +%s)"
enqueue_prepare "$PREPARE_JOB_ID"
poll_job "$PREPARE_JOB_ID" "prepare_factorization"
PREPARE_DONE_EPOCH="$(date +%s)"

echo "[info] enqueue solve job: $SOLVE_JOB_ID (demand_process_idx=$DEMAND_PROCESS_IDX)"
SOLVE_ENQUEUE_EPOCH="$(date +%s)"
enqueue_solve "$SOLVE_JOB_ID"
poll_job "$SOLVE_JOB_ID" "solve_one"
SOLVE_DONE_EPOCH="$(date +%s)"

IFS='|' read -r RESULT_ID RESULT_ARTIFACT_FORMAT RESULT_ARTIFACT_SIZE RESULT_ARTIFACT_URL RESULT_HAS_INLINE_PAYLOAD < <(
  sql_scalar "
  SELECT
    id::text,
    COALESCE(artifact_format, ''),
    COALESCE(artifact_byte_size::text, ''),
    COALESCE(artifact_url, ''),
    CASE WHEN payload IS NOT NULL THEN 'true' ELSE 'false' END
  FROM public.lca_results
  WHERE job_id = '$SOLVE_JOB_ID'::uuid
  ORDER BY created_at DESC
  LIMIT 1;
  "
)

echo "[info] latest solve result row:"
sql_exec "
SELECT
  id,
  job_id,
  snapshot_id,
  artifact_format,
  artifact_byte_size,
  artifact_url,
  (payload IS NOT NULL) AS has_inline_payload,
  created_at
FROM public.lca_results
WHERE job_id = '$SOLVE_JOB_ID'::uuid
ORDER BY created_at DESC
LIMIT 1;
"

echo "[info] worker log tail:"
tail -n 120 "$WORKER_LOG" || true

if [ "$KEEP_WORKER_ALIVE" = "1" ]; then
  echo "[info] keep-worker-alive enabled; worker pid=$WORKER_PID remains running"
fi

echo "[done] full compute debug run finished"
