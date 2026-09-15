#!/usr/bin/env bash
# Result120 numerical-isolation acceptance runner (Worker #289).
#
# This runner NEVER creates, resets, migrates, starts or stops a database, storage service, or
# container. It selects exactly ONE scenario per invocation and runs it against a task instance the
# coordinator has already prepared and will reset between scenarios.
#
# Why one scenario per invocation: every scenario seeds immutable authored rows (a published Result
# at state 120 and approved public rows at state 100). The domain guards protect those rows against
# UPDATE and DELETE for every role, and this suite must not relax them to make itself idempotent.
# Lifecycle is therefore: coordinator resets the dedicated task instance -> runner executes one
# scenario -> runner writes a fixture manifest naming every retained row/object.
#
# Prerequisites (all must be supplied; there are no embedded defaults)
# ------------------------------------------------------------------
#   RESULT120_CONFIRM_ISOLATED=I_CONFIRM_ISOLATED_TASK_DATABASE
#   RESULT120_TASK_INSTANCE_ID   a task-specific identity used in the bucket-name check
#   RESULT120_TASK_FIXTURE_FRESH=I_CONFIRM_FRESH_TASK_FIXTURE
#                                coordinator reset the dedicated task instance; asserted against
#                                empty public.processes/public.flows before any fixture insert
#   RESULT120_EGRESS_SINK_CONFIRMED=I_CONFIRM_TASK_EGRESS_SINK
#                                `process_extract_md_trigger_insert` (AFTER INSERT, no WHEN) calls
#                                util.project_url()/util.project_secret_key() and POSTs to an Edge
#                                function. A task instance is expected to point project_url at a
#                                loopback blackhole *inside the DB container* with a dummy
#                                project_secret_key, so the trigger completes transactionally and no
#                                hosted egress occurs. The Rust preflight only asserts those helpers
#                                are CONFIGURED before the first fixture row is written; it opens no
#                                connection and is not reachability proof.
#   RESULT120_SCENARIO           exactly one of the scenario names listed below
#                                (the runner builds and pins `SNAPSHOT_BUILDER_BIN` for the
#                                scenarios that spawn the snapshot builder; no TIDAS binary is
#                                required because this suite never runs package import)
#   RESULT120_DATABASE_URL       loopback Postgres URL of the dedicated task instance
#
#   For network scenarios (`full`) also:
#     S3_ENDPOINT                loopback object-storage endpoint (no default)
#     S3_REGION                  explicit region string
#     S3_BUCKET                  bucket whose name contains RESULT120_TASK_INSTANCE_ID
#     S3_ACCESS_KEY_ID           explicit credential (no default)
#     S3_SECRET_ACCESS_KEY       explicit credential (no default)
#
# Scenarios
# ---------
#   numerical-isolation   DB -> provider selection -> matrix axes -> solver stability for U(P)/R(P)
#   owner-scope           owner draft path must not re-admit a published Result
#   review-diagnostic     Review Admin diagnostic keeps state 20 and excludes the Result
#   product-export        published Result excluded from package export seeds/roots/hydration
#   database-only         NO storage required: Database predicates + export fence predicate only.
#                         This is partial proof and never substitutes for the scenarios above.
#
# The Rust entrypoints re-validate every guard, because `cargo test -- --ignored` bypasses this
# script entirely.
set -euo pipefail

worker_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${RESULT120_CONFIRM_ISOLATED:-}" != "I_CONFIRM_ISOLATED_TASK_DATABASE" ]]; then
  echo "refusing to run: set RESULT120_CONFIRM_ISOLATED=I_CONFIRM_ISOLATED_TASK_DATABASE" >&2
  exit 2
fi

if [[ -z "${RESULT120_TASK_INSTANCE_ID:-}" ]]; then
  echo "refusing to run: RESULT120_TASK_INSTANCE_ID must name the dedicated task instance" >&2
  exit 2
fi

if [[ -z "${RESULT120_DATABASE_URL:-}" ]]; then
  echo "refusing to run: RESULT120_DATABASE_URL is required (dedicated task instance)" >&2
  exit 2
fi

scenario="${RESULT120_SCENARIO:-}"
case "$scenario" in
  numerical-isolation) test_name="published_result_does_not_change_unit_numerical_result" ;;
  owner-scope)         test_name="owner_scope_does_not_readmit_a_published_result" ;;
  review-diagnostic)   test_name="review_diagnostic_scope_keeps_review_state_and_excludes_result" ;;
  product-export)      test_name="published_result_is_excluded_from_product_export" ;;
  database-only)       test_name="database_only_eligibility_and_export_fences" ;;
  *)
    echo "refusing to run: RESULT120_SCENARIO must be one of" >&2
    echo "  numerical-isolation | owner-scope | review-diagnostic | product-export | database-only" >&2
    echo "one scenario per invocation; the coordinator resets the task instance between scenarios" >&2
    exit 2
    ;;
esac

case "$RESULT120_DATABASE_URL" in
  *localhost*|*127.0.0.1*|*\[::1\]*) ;;
  *)
    echo "refusing to run: RESULT120_DATABASE_URL must point at a loopback task instance" >&2
    exit 2
    ;;
esac

export DATABASE_URL="$RESULT120_DATABASE_URL"
export RESULT120_CONFIRM_ISOLATED
export RESULT120_TASK_INSTANCE_ID

if [[ "${RESULT120_TASK_FIXTURE_FRESH:-}" != "I_CONFIRM_FRESH_TASK_FIXTURE" ]]; then
  echo "refusing to run: set RESULT120_TASK_FIXTURE_FRESH=I_CONFIRM_FRESH_TASK_FIXTURE after" >&2
  echo "resetting the dedicated task instance; the suite never deletes authored immutable rows" >&2
  exit 2
fi

if [[ "${RESULT120_EGRESS_SINK_CONFIRMED:-}" != "I_CONFIRM_TASK_EGRESS_SINK" ]]; then
  echo "refusing to run: set RESULT120_EGRESS_SINK_CONFIRMED=I_CONFIRM_TASK_EGRESS_SINK once the" >&2
  echo "the task instance is prepared for public.processes inserts (process_extract_md_trigger_insert);" >&2
  echo "this asserts the webhook helpers are configured, not that the sink endpoint is reachable" >&2
  exit 2
fi
export RESULT120_TASK_FIXTURE_FRESH RESULT120_EGRESS_SINK_CONFIRMED

# The suite only needs the task database for the freshness/egress preflight; it never reaches the
# shared local stack. Row counts are read by the Rust preflight, not here.

if [[ "$scenario" != "database-only" ]]; then
  for name in S3_ENDPOINT S3_REGION S3_BUCKET S3_ACCESS_KEY_ID S3_SECRET_ACCESS_KEY; do
    if [[ -z "${!name:-}" ]]; then
      echo "refusing to run: $name must be supplied explicitly for scenario '$scenario'" >&2
      echo "there are no embedded endpoints or credentials" >&2
      exit 2
    fi
  done
  case "$S3_ENDPOINT" in
    *localhost*|*127.0.0.1*|*\[::1\]*) ;;
    *)
      echo "refusing to run: S3_ENDPOINT must be a loopback task endpoint" >&2
      exit 2
      ;;
  esac
  if [[ "$S3_BUCKET" != *"$RESULT120_TASK_INSTANCE_ID"* ]]; then
    echo "refusing to run: S3_BUCKET must contain RESULT120_TASK_INSTANCE_ID" >&2
    exit 2
  fi
  export S3_ENDPOINT S3_REGION S3_BUCKET S3_ACCESS_KEY_ID S3_SECRET_ACCESS_KEY
fi

cd "$worker_root"

# The suite spawns snapshot_builder as a child process, so the binary must reflect the current
# sources. Build it explicitly and hand over an absolute path; the Rust preflight then re-checks the
# path is absolute, executable, and not older than the Worker sources, so a stale `target/debug`
# executable can never be accepted silently.
if [[ "$scenario" != "product-export" && "$scenario" != "database-only" ]]; then
  cargo build -p solver-worker --bin snapshot_builder
  export SNAPSHOT_BUILDER_BIN="$worker_root/target/debug/snapshot_builder"
  export LCA_WORKER_ROOT="$worker_root"
  if [[ ! -x "$SNAPSHOT_BUILDER_BIN" ]]; then
    echo "refusing to run: $SNAPSHOT_BUILDER_BIN was not produced by the build" >&2
    exit 2
  fi
fi

# The product-export scenario runs the real export handler and asserts the actual ZIP bytes it
# produced; it needs no snapshot builder and no TIDAS binary, because `execute_export_package` never
# invokes TIDAS. Only the import path does, and this suite does not import.

# `--test-threads=1` is a second serialization layer; the Rust guard also holds a process-wide mutex
# and refuses to start when the instance already has queued/running work.
cargo test -p solver-worker --test result120_numerical_isolation -- --ignored --nocapture \
  --test-threads=1 "$test_name"
