#!/usr/bin/env bash
set -euo pipefail

# Explicit loopback-only disposable database. This never resets a supplied DB.
: "${WORKER_CLAIM_TEST_PORT:?Set the port of an isolated local PostgreSQL server}"
[[ "$WORKER_CLAIM_TEST_PORT" =~ ^[0-9]+$ ]] || exit 2
export PGHOST=127.0.0.1 PGPORT="$WORKER_CLAIM_TEST_PORT" PGUSER=postgres
export PGCONNECT_TIMEOUT=5 PGSSLMODE=disable
unset PGSERVICE PGSERVICEFILE
export WORKER_CLAIM_TEST_DATABASE="worker_claim_test_$(date +%s)_$$"
createdb "$WORKER_CLAIM_TEST_DATABASE"
cleanup() { dropdb --if-exists "$WORKER_CLAIM_TEST_DATABASE"; }
trap cleanup EXIT
cargo test -p solver-worker --lib --all-features worker_jobs::claim::tests::postgres_fault_recovery -- --ignored --exact --nocapture
