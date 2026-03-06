# AGENTS.md

This document is for AI coding agents working in this repository.

## 0. Mandatory maintenance rule

`AGENTS.md` is a living contract.
Every time code/schema/runtime behavior changes in this repo, update this file in the same task.

Must always stay aligned with:

- architecture boundaries
- API/payload/result contracts
- schema + migration expectations
- runtime/ops workflows
- current TODO + risks

## 1. Project goal

Build a production LCA sparse solver stack with strict separation:

- Supabase: source data, auth, queues (`pgmq`), runtime metadata.
- Rust worker: matrix load, factorization, solve, persistence.
- SuiteSparse: numeric backend (UMFPACK via Rust FFI).

Hard invariants:

- Always solve `M x = y` where `M = I - A`.
- Never compute explicit inverse.
- Heavy compute must stay async (queue worker path).
- Scope is full-library process network compute (`lifecyclemodels` not in numeric solve).

## 2. Current architecture (2026-03-06)

### 2.1 Rust workspace

- `crates/suitesparse-ffi`
  - CSC matrix representation + UMFPACK FFI wrappers.
- `crates/solver-core`
  - matrix build/validate, factorization cache, solve orchestration.
- `crates/solver-worker`
  - queue consumer + internal HTTP + DB/object-storage persistence.

### 2.2 Worker responsibilities

- consumes `pgmq` queue `lca_jobs`
- executes:
  - `prepare_factorization`
  - `solve_one`
  - `solve_batch`
  - `invalidate_factorization`
  - `rebuild_factorization`
- updates `lca_jobs` status/diagnostics
- writes `lca_results` rows (artifact metadata only)
- updates request cache state in `lca_result_cache` by `job_id`

### 2.3 Snapshot builder

Canonical entry:

- `crates/solver-worker/src/bin/snapshot_builder.rs`
- wrapper: `scripts/build_snapshot_from_ilcd.sh`

Behavior:

- builds sparse payload from `processes/flows/lciamethods`
- writes snapshot artifact (`snapshot-hdf5:v1`) to S3
- writes metadata to `lca_network_snapshots` + `lca_snapshot_artifacts`
- emits coverage report (`reports/snapshot-coverage/...`)
- supports same-source skip-rebuild via source fingerprint (`count + max(modified_at) + config`)

## 3. Storage/result policy (strict)

## 3.1 Solve result persistence

Solve results are **S3-only**.

- format: `hdf5:v1`
- container: HDF5
- compression: chunked `deflate` (zlib level 4) on `envelope_json`
- checksum: SHA-256 hex

`lca_results` stores only metadata + diagnostics:

- `artifact_url` (required)
- `artifact_sha256` (required)
- `artifact_byte_size` (required)
- `artifact_format` (required, currently `hdf5:v1`)
- `diagnostics`

Inline JSON result payload is removed and not supported.

## 3.2 Retention + GC

Retention fields on `lca_results`:

- `expires_at`
- `is_pinned`

GC tool:

- binary: `cargo run -p solver-worker --bin result_gc --release -- ...`
- wrapper: `scripts/gc_lca_results.sh`

GC delete policy:

- only expired rows (`expires_at < now()`)
- skip pinned rows (`is_pinned=true`)
- skip rows referenced by active cache (`lca_result_cache` in `pending/running/ready`)
- keep latest 1 row per request partition:
  - partition key: `requested_by + snapshot_id + coalesce(request_key, job_id)`
  - delete only rows with `row_number > 1`

Delete order:

1. delete S3 object
2. delete DB row

## 4. Schema + migration baseline

Applied/expected migrations:

- `20260304073000_lca_snapshot_phase1.sql`
- `20260304103000_lca_snapshot_artifacts.sql`
- `20260304120000_lca_drop_legacy_entry_tables.sql`
- `20260305052000_lca_request_cache_and_factorization_registry.sql`
- `20260305070000_lca_rls_lockdown.sql`
- `20260305093000_lca_enqueue_job_rpc.sql`
- `20260305094000_lca_enqueue_job_rpc_acl.sql`
- `20260306090000_lca_results_s3_strict_and_retention.sql` (destructive for old results)

Current runtime tables:

- `lca_network_snapshots`
- `lca_snapshot_artifacts`
- `lca_jobs`
- `lca_results`
- `lca_active_snapshots`
- `lca_result_cache`
- `lca_factorization_registry` (schema ready, runtime usage limited)

## 5. Security/permission baseline

- `lca_*` tables have RLS enabled.
- `anon` has no direct table access.
- `authenticated` can read only own `lca_jobs` and associated `lca_results`.
- enqueue path must use service-side RPC:
  - `public.lca_enqueue_job(text, jsonb)`
  - execute granted to `service_role` only.

## 6. Cross-project contracts

Authoritative docs in this repo:

- `docs/lca-api-contract.md`
- `docs/edge-function-integration.md`
- `docs/frontend-integration.md`

Current contract highlights:

- queue payload uses `snapshot_id` (`model_version` accepted only as worker alias)
- Edge submits jobs via RPC (`lca_enqueue_job`), not direct DB driver `pgmq.send`
- result fetch contract is artifact metadata only (no inline result field)

## 7. Runtime env expectations

Required DB env:

- `DATABASE_URL` (preferred) or `CONN`

Required S3 env for worker/runtime:

- `S3_ENDPOINT`
- `S3_REGION`
- `S3_BUCKET`
- `S3_ACCESS_KEY_ID`
- `S3_SECRET_ACCESS_KEY`

Optional:

- `S3_SESSION_TOKEN`
- `S3_PREFIX` (default `lca-results`)

Note:

- Worker startup is expected to fail fast if required S3 config is missing.

## 8. Operations runbook

Build/check:

```bash
make check
```

Run worker (local):

```bash
set -a && source .env && set +a
cargo run -p solver-worker --release -- --mode worker
```

Recommended production mode:

- `systemd` + release binary (multi-instance)
- keep `WORKER_VT_SECONDS` above slow-job runtime

Snapshot build:

```bash
./scripts/build_snapshot_from_ilcd.sh
```

Full compute debug:

```bash
./scripts/run_full_compute_debug.sh --snapshot-id <snapshot_id>
```

Result GC:

```bash
./scripts/gc_lca_results.sh
./scripts/gc_lca_results.sh --dry-run
```

Manual Brightway validation:

```bash
./scripts/run_bw25_validation.sh --snapshot-id <snapshot_id>
```

## 9. Validation tool status

`tools/bw25-validator` is manual-only and out-of-band.

- validates `solve_one`
- reads snapshot/result artifacts from S3 (`snapshot-hdf5:v1` + `hdf5:v1`)
- compares Rust vs Brightway (`x/g/h`, residuals, timing)
- Linux x64 installs `pypardiso` to remove warning and speed sparse solves

## 10. Current TODO (priority)

P0:

- integrate snapshot build as queue job (`build_snapshot`) instead of CLI-only path
- add integration tests for artifact-first end-to-end flow
- add CI smoke for `bw25-validator` with fixture artifact pair
- keep three-repo contract synced (calculator / edge / frontend)

P1:

- retry/backoff/dead-letter strategy for failed jobs
- structured queue lag / throughput metrics
- explicit diagnostics schema versioning
- distributed factorization coordination if/when needed

P2:

- optional CHOLMOD/SPQR backend abstraction
- richer contribution/post-processing outputs

## 11. Known risks

- Same-source snapshot reuse depends on source table `modified_at` triggers being reliable.
- Factorization cache is process-local only; restart loses warm cache.
- Internal HTTP endpoints are for trusted internal network only.
- Snapshot artifact fallback to legacy `lca_*` tables is compatibility code path; those tables may not exist after cleanup migration.

## 12. Done criteria for agent tasks

A task is done only when:

- code compiles
- checks/tests for touched area pass (or failure is explicitly reported)
- contracts/docs are synced (`AGENTS.md` + README + docs if affected)
- no stale legacy behavior is left undocumented
