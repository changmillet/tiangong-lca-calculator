# AGENTS.md

This file is for AI coding agents working in this repository.

## 0. Maintenance policy (mandatory)

`AGENTS.md` is a living contract.  
For every relevant code/schema/behavior change in this repo, update this file in the same task.

Always sync:

- architecture/module boundaries
- API/payload contracts
- schema assumptions
- dependency/runtime/tooling changes
- implementation status
- TODO priorities and known risks

Do not leave this file stale.

## 1. Project intent

Build a high-throughput LCA sparse solver stack with strict separation:

- Supabase: source data + orchestration + queue (`pgmq`) + job/result persistence.
- Rust solver-worker: sparse matrix build/validation/factorization/solve/writeback.
- SuiteSparse backend: UMFPACK via Rust FFI (current backend).

Core invariants:

- Always solve `M x = y` with `M = I - A`.
- Never compute explicit inverse.
- Heavy compute only in async worker path.
- Active scope: full-library process network solve (`lifecyclemodels` excluded from numeric solve path).

## 2. Current status (as of 2026-03-05)

### 2.1 Implemented

- Cargo workspace with 3 crates:
  - `crates/suitesparse-ffi`
  - `crates/solver-core`
  - `crates/solver-worker`
- UMFPACK minimal binding + safe wrappers:
  - `CscMatrix`
  - `UmfpackFactorization`
- Core pipeline:
  - build `M`, `B`, `C`
  - structure validation
  - in-memory factorization cache
  - `solve_one` / `solve_batch`
  - timed solve breakdown for `solve_one` (`solve_mx_sec`, `bx_sec`, `cg_sec`, `comparable_compute_sec`)
  - result persistence timing breakdown (`encode_artifact_sec`, `upload_artifact_sec`, `db_write_sec`, `total_sec`)
  - result/snapshot HDF5 artifacts use chunked `deflate` compression (level 4) on `envelope_json`
  - job status DB write timing in `lca_jobs.diagnostics.job_status_update_timing_sec` (`running/ready/completed/failed` + `last_*`)
  - benchmark persist mode switch (`normal` / `inline-only`)
  - solve output assembly avoids eager evaluation for unrequested vectors (`return_x/return_g/return_h`)
  - normal persist path uses lazy JSON serialization (serialize only when inline path is actually used)
- Worker/API:
  - pgmq queue consume + archive
  - job execution for `prepare_factorization`, `solve_one`, `solve_batch`, `invalidate_factorization`, `rebuild_factorization`
  - internal HTTP endpoints (snapshot-first):
    - `POST /internal/snapshots/{snapshot_id}/prepare`
    - `GET /internal/snapshots/{snapshot_id}/factorization`
    - `POST /internal/snapshots/{snapshot_id}/solve`
    - `POST /internal/snapshots/{snapshot_id}/invalidate`
  - backward-compatible aliases:
    - `/internal/models/{snapshot_id}/...`
- Payload contract migrated to `snapshot_id` with backward alias support:
  - `model_version` is accepted as serde alias for queue payloads.

### 2.2 Supabase schema/migration status

Applied migrations:

- `supabase/migrations/20260304073000_lca_snapshot_phase1.sql` (additive)
- `supabase/migrations/20260304103000_lca_snapshot_artifacts.sql` (additive)
- `supabase/migrations/20260304120000_lca_drop_legacy_entry_tables.sql` (cleanup drop)

Created tables:

- `lca_network_snapshots`
- `lca_snapshot_artifacts`
- `lca_jobs`
- `lca_results`

Legacy phase1 matrix/index tables are removed by cleanup migration:

- `lca_process_index` (dropped)
- `lca_flow_index` (dropped)
- `lca_technosphere_entries` (dropped)
- `lca_biosphere_entries` (dropped)
- `lca_characterization_factors` (dropped)

Created queue:

- `pgmq.meta.queue_name = 'lca_jobs'`

Verification done at migration time:

- Existing source tables row counts unchanged (`processes`, `flows`, `lciamethods`, `lifecyclemodels`).
- Cleanup migration only touches legacy `lca_*` intermediate tables.

### 2.3 Result storage policy (implemented)

Hybrid persistence is now active in `solver-worker`:

- Always encode result artifact as:
  - `HDF5`
  - format id: `hdf5:v1`
  - extension: `.h5`
  - checksum: SHA-256 (hex)
  - compression: `HDF5 deflate` filter (zlib, level 4, chunked dataset)
- If encoded bytes `< RESULT_INLINE_MAX_BYTES` (default 256KB):
  - store JSON payload inline in `lca_results.payload`
- If encoded bytes `>= RESULT_INLINE_MAX_BYTES` and S3 config exists:
  - upload artifact to object storage
  - store metadata in `lca_results`:
    - `artifact_url`
    - `artifact_sha256`
    - `artifact_byte_size`
    - `artifact_format`
- If upload fails:
  - fallback to inline JSON payload and warn in logs
- `RESULT_PERSIST_MODE`:
  - `normal` (default): current hybrid policy
  - `inline-only`: skip artifact encode/upload, always write inline JSON (benchmark mode)
- Result diagnostics include:
  - `compute_timing_sec` for comparable compute lane
  - `persistence_timing_sec` for result-write split timing

Object storage config keys:

- `S3_ENDPOINT`
- `S3_REGION`
- `S3_BUCKET`
- `S3_ACCESS_KEY_ID`
- `S3_SECRET_ACCESS_KEY`
- optional `S3_SESSION_TOKEN`
- optional `S3_PREFIX` (default `lca-results`)

Uploads are authenticated with AWS SigV4 (`Authorization` + `x-amz-date` + `x-amz-content-sha256`).

`HDF5` build mode in this repo uses `hdf5-sys(static,zlib)`; build host must provide `cmake`.

`DATABASE_URL` is preferred DB env var; `CONN` is accepted fallback.

### 2.4 Validation state

Latest checks passed:

- `cargo fmt --all`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `python3 -m py_compile tools/bw25-validator/src/bw25_validator/cli.py`
- `uv run --project tools/bw25-validator python -c "import pypardiso"` (Linux x86_64)
- `./scripts/run_bw25_validation.sh --report-dir reports/bw25-validation-smoke` (manual smoke, latest solve_one target)
- `./scripts/run_full_compute_debug.sh --snapshot-id 6201b08a-b125-43a1-b4b8-aacf5a493987` + manual bw25 validation confirms comparable-compute speed lane reporting.
- `bash -n scripts/cleanup_local_artifacts.sh` (cleanup helper syntax check)
- `./scripts/run_full_compute_debug.sh --report-dir reports/full-run-persistence-split --log-dir logs/full-run-persistence-split`
- `./scripts/run_bw25_validation.sh --result-id 1b0a8cdd-2b08-45e4-85a6-43255ae6ecc0 --report-dir reports/bw25-validation-persistence-split`
- `./scripts/run_full_compute_debug.sh --result-persist-mode inline-only --report-dir reports/full-run-inline-only --log-dir logs/full-run-inline-only`
- `./scripts/run_bw25_validation.sh --result-id 3c06c0c4-c846-46dc-b8f8-d9343aff15ce --report-dir reports/bw25-validation-inline-only`
- `./scripts/run_full_compute_debug.sh --result-persist-mode inline-only --report-dir reports/full-run-ms-precision --log-dir logs/full-run-ms-precision`
- `./scripts/run_bw25_validation.sh --result-id 50f6f0c2-863a-49df-ba63-383ac77d51e7 --report-dir reports/bw25-validation-ms-precision`
- `./scripts/run_full_compute_debug.sh --result-persist-mode inline-only --report-dir reports/full-run-step4 --log-dir logs/full-run-step4`
- `./scripts/run_bw25_validation.sh --result-id 18157632-551e-457b-98df-4a420faacc18 --report-dir reports/bw25-validation-step4`
- `RESULT_PERSIST_MODE=inline-only ./scripts/run_full_compute_debug.sh --report-dir reports/full-run-step5` (queue + DB write telemetry smoke)
- `./scripts/run_full_compute_debug.sh --report-dir reports/full-run-step6` (normal mode, compressed artifact smoke)
- `./scripts/run_bw25_validation.sh --result-id 1b49f6eb-6d46-43da-bd61-d56c78b393cc --report-dir reports/bw25-validation-step6`

### 2.7 Repository hygiene/docs organization (implemented)

- Added optimization assessment doc:
  - `OPTIMIZATION_REVIEW.md`
- Added local artifact cleanup helper:
  - `scripts/cleanup_local_artifacts.sh`
  - supports `--dry-run`, `--with-target`
- `.gitignore` now explicitly excludes local runtime outputs:
  - `/logs/`
  - `/reports/`

### 2.5 Snapshot builder status (artifact-first)

- `crates/solver-worker/src/bin/snapshot_builder.rs` is the canonical builder implementation.
- `scripts/build_snapshot_from_ilcd.sh` is now a thin wrapper that calls:
  - `cargo run -p solver-worker --bin snapshot_builder --release -- ...`
- Builder default behavior:
  - default `--process-states 100` (only `state_code=100`)
  - default `--process-limit 0` (no limit)
  - supports `--process-states all` to disable `state_code` filtering entirely
- Builder output:
  - uploads snapshot matrix artifact to S3 (`snapshot-hdf5:v1`, HDF5 deflate compressed)
  - writes metadata to `lca_network_snapshots` and `lca_snapshot_artifacts`
  - writes coverage report:
    - `reports/snapshot-coverage/<snapshot_id>.json`
    - `reports/snapshot-coverage/<snapshot_id>.md`
- Snapshot coverage report includes build timing phases:
  - resolve method identity
  - source fingerprint
  - reuse lookup
  - method factor load
  - sparse payload build
  - encode/upload/persist
  - total
- Builder now supports same-source skip-rebuild:
  - computes source fingerprint from `processes/flows/lciamethods` as `count(*) + max(modified_at)` plus build config
  - looks up `lca_network_snapshots.source_hash` + ready snapshot artifact
  - on hit, reuses existing snapshot artifact and returns immediately
  - explicit `--snapshot-id` disables auto-reuse and forces build for that ID
- Builder performance updates:
  - flow metadata fetch is candidate-id scoped (`WHERE id = ANY(...)`, latest row per id)
  - process exchange parsing runs in parallel (rayon) across process shards
- Coverage report metrics include:
  - matching coverage (`input_edges_total`, unique/multi/unmatched, unique/any match pct)
  - singular risk (`prefilter_diag_abs_ge_cutoff`, `postfilter_a_diag_abs_ge_cutoff`, `m_zero_diagonal_count`, `m_min_abs_diagonal`, derived risk level)
  - matrix scale (`process_count`, `flow_count`, `impact_count`, `a_nnz`, `b_nnz`, `c_nnz`, `m_nnz_estimated`, `m_sparsity_estimated`)
- `scripts/run_full_compute_debug.sh` now writes one run report per execution and reads matrix scale from `lca_snapshot_artifacts` first (fallback to legacy tables):
  - default `reports/full-run/run-<ts>.json`
  - default `reports/full-run/run-<ts>.md`
  - includes nanosecond-sampled local timing (seconds with 6 decimals), plus merged `build_snapshot` and `build_and_compute_total`, job ids/status, matrix nnz summary, artifact metadata, result compute/persistence timing split, log paths.
  - includes DB-derived job timing:
    - `job_timing_sec.prepare.{queue_wait,run,end_to_end}`
    - `job_timing_sec.solve.{queue_wait,run,end_to_end}`
    - `job_db_write_timing_sec.{prepare,solve}.{running,ready,completed,failed,last,last_status}`
    - UTC timestamps under `jobs.{prepare_*,solve_*}`
  - auto-discovers latest snapshot coverage report by `snapshot_id` and attaches build source metadata (`reused_snapshot`, `build_report_json`) into full-run report.

### 2.6 Brightway25 validation path (manual-only)

- Added standalone Python validator under `tools/bw25-validator`.
- Validation is opt-in and never auto-runs in worker/job path.
- Manual entrypoint:
  - `scripts/run_bw25_validation.sh`
- Current scope:
  - validates `solve_one` jobs only
  - loads snapshot from `lca_snapshot_artifacts` (`snapshot-hdf5:v1`)
  - loads solve result from `lca_results.payload` or result artifact (`hdf5:v1`)
  - reconstructs `M` in Python and runs Brightway `LCA(..., data_objs=[...])`
  - compares Rust vs Brightway vectors (`x/g/h`) and writes report:
    - `reports/bw25-validation/<result_id>.json`
    - `reports/bw25-validation/<result_id>.md`
  - includes speed comparison in report/log:
    - Rust job timing (`queue_wait_sec`, `run_sec`, `end_to_end_sec`) from `lca_jobs`
    - Rust comparable compute timing from `lca_results.diagnostics.compute_timing_sec`
    - Rust persistence timing from `lca_results.diagnostics.persistence_timing_sec`
    - Brightway timing (`solve_sec`, `build_plus_solve_sec`)
    - ratio fields and faster-side summary (prefer comparable-compute ratio when available)
- Validator package/runtime:
  - `brightway25==1.1.1` (PyPI latest as of 2026-03-04)
  - `bw2calc`, `bw_processing`, `numpy`, `scipy`, `h5py`, `psycopg`, `boto3`, `requests`
  - `pypardiso>=0.4.6` auto-installed on `Linux x86_64` for faster Brightway sparse solve and to remove x64 warning
  - runner prefers `uv run --project tools/bw25-validator`, falls back to local virtualenv install.
- Speed comparison behavior:
  - prefers comparable-compute lane when available:
    - Rust `comparable_compute_sec = solve_mx_sec + bx_sec + cg_sec`
    - Brightway `solve_sec` and `build_plus_solve_sec`
  - keeps job-run lane (`run_sec`) for end-to-end overhead insight.

## 3. Architecture map

### 3.1 `crates/suitesparse-ffi`

- `src/matrix.rs`:
  - internal CSC representation
  - COO->CSC conversion + dedup + zero pruning
  - structural checks + sparse mat-vec
- `src/umfpack.rs`:
  - raw FFI declarations
  - symbolic/numeric solve wrappers
  - `Drop`-based C resource cleanup

### 3.2 `crates/solver-core`

- `src/data_builder.rs`: build `M/B/C` from sparse entries
- `src/validator.rs`: pre-factorization checks/warnings
- `src/cache.rs`: in-memory factorization cache/state
- `src/service.rs`: `prepare/solve/invalidate` orchestration
  - provides timed solve API for comparable compute benchmarking
  - no eager default-vector construction for unrequested `g` output

### 3.3 `crates/solver-worker`

- `src/config.rs`:
  - env/CLI config (`DATABASE_URL` + `CONN` fallback)
  - queue/http settings
  - object-storage settings
  - result persist mode (`RESULT_PERSIST_MODE`)
- `src/db.rs`:
  - reads snapshot sparse data from `lca_snapshot_artifacts` first
  - fallback reads from legacy `lca_*` entry tables
  - updates `lca_jobs`
  - records per-status `lca_jobs` DB write latency in `diagnostics.job_status_update_timing_sec`
  - writes `lca_results` payload/metadata
  - stores `solve_one` compute timings in `lca_results.diagnostics.compute_timing_sec`
  - stores result persistence split timings in `lca_results.diagnostics.persistence_timing_sec`
  - supports benchmark persist mode `inline-only` (skip encode/upload)
  - delays `serde_json::to_value(...)` for normal mode until inline path is required (small result or upload fallback)
- `src/artifacts.rs`:
  - artifact envelope encode (`hdf5:v1`)
  - `envelope_json` dataset uses chunked `deflate` compression (level 4)
  - SHA-256 checksum
- `src/snapshot_artifacts.rs`:
  - snapshot artifact encode/decode (`snapshot-hdf5:v1`)
  - `envelope_json` dataset uses chunked `deflate` compression (level 4)
  - snapshot build config + coverage metadata model
- `src/storage.rs`:
  - S3-compatible upload/download client (path-style URL)
  - SigV4 for PUT and private-bucket GET
- `src/queue.rs`:
  - queue polling + message lifecycle
- `src/http.rs`:
  - internal snapshot/model alias routes
- `src/types.rs`:
  - queue/API payload contracts
- `src/bin/snapshot_builder.rs`:
  - source-table extraction + `A/B/C` build + coverage + artifact upload + metadata persist
- `scripts/cleanup_local_artifacts.sh`:
  - removes local generated runtime artifacts (`logs/`, `reports/`, `tools/bw25-validator/.venv/`)
  - optional `--with-target` to also remove Rust `target/`

### 3.4 `tools/bw25-validator` (manual validation tool)

- `src/bw25_validator/cli.py`:
  - resolves target result row (`--result-id` / `--job-id` / latest `solve_one`)
  - reads snapshot/result artifacts and decodes HDF5 envelopes
  - rebuilds sparse matrices with SciPy
  - runs Brightway `LCA` using datapackage vectors
  - computes vector deltas/residuals and writes JSON/MD report
- `scripts/run_bw25_validation.sh`:
  - manual launcher
  - auto-loads `.env`
  - uses `uv` if present, otherwise creates `.venv/bw25-validator`

## 4. Schema assumptions

Current runtime primary path expects:

- `lca_network_snapshots`
- `lca_snapshot_artifacts`
- `lca_jobs`
- `lca_results`

`lca_network_snapshots.source_hash` is used as source fingerprint key for skip-rebuild matching.

Legacy fallback path in code exists for compatibility, but current DB may not have those tables after cleanup migration.

Input source-of-truth upstream remains:

- `processes`
- `flows`
- `lciamethods`

`lifecyclemodels` is not part of numeric solve.

## 5. Known limitations / risks

- Snapshot builder is a standalone Rust binary, not yet a queue job type integrated into worker runtime.
- Same-source skip depends on `modified_at` correctness on source tables (`processes/flows/lciamethods`) plus row counts.
- Provider matching in snapshot builder is flow-based (`strict_unique_provider`), because source exchange JSON usually lacks stable provider-process references; this can reduce technosphere edge coverage.
- Factorization cache is process-local memory only.
- No persisted factorization snapshots across restart.
- Internal HTTP endpoints do not enforce auth (assumed trusted internal network).
- Current uploader supports static key/secret (+ optional session token) credentials; key rotation and STS refresh are not yet automated.
- No advanced contribution/post-processing yet.
- Backend is UMFPACK-only; CHOLMOD/SPQR not exposed yet.
- Brightway validator is manual-only by design and currently validates `solve_one` (not `solve_batch` aggregate logic).
- Brightway validation assumes snapshot/result artifact schema `v1` (`snapshot-hdf5:v1`, `hdf5:v1`).
- Compression now depends on HDF5 `deflate` filter availability; runtime encoding fails if binary is built without zlib-enabled HDF5.
- `job_status_update_timing_sec.*` is measured client-side around the main `UPDATE lca_jobs ... status` call and then persisted via a diagnostics-only follow-up update.
- Current `persistence_timing_sec.db_write_sec` measures `INSERT lca_results` latency; diagnostics are finalized with a follow-up `UPDATE`, which is not included in `db_write_sec`.
- `inline-only` benchmark mode still writes full JSON payload to `lca_results`; for very large vectors this can increase DB row size/IO.
- `timing_sec.prepare_job/solve_job` are orchestrator wall-clock spans and intentionally differ from DB `job_timing_sec.*` (which isolates queue wait/run/end-to-end from DB timestamps).

## 6. TODO backlog (priority)

### P0

- Integrate snapshot builder into queue job (`build_snapshot`) so it can be triggered from Supabase jobs, not only CLI.
- Add integration tests for artifact-first path:
  - build snapshot artifact
  - prepare/solve from artifact
  - verify fallback behavior when artifact unavailable
- Keep `scripts/run_full_compute_debug.sh` aligned with artifact-first schema as contracts evolve.
- Add integration tests with real Postgres + `pgmq` (containerized).
- Add artifact reader/decoder utilities for both:
  - result format `hdf5:v1`
  - snapshot format `snapshot-hdf5:v1`
- Add CI smoke for `tools/bw25-validator` (fixture snapshot + fixture solve result, threshold assertions).
- Strengthen job/result diagnostics schema:
  - factorization stats
  - timing breakdown
  - failure code taxonomy
- Refine result persistence timing to split `insert_result` vs diagnostics `update_result` overhead in one consistent metric model.
- Optional: add `no-write` benchmark mode for pure compute profiling when persistence is intentionally bypassed.

### P1

- Add retry/backoff/dead-letter flow for failed jobs.
- Add cache TTL/eviction and memory pressure controls.
- Add structured metrics and queue lag observability.
- Add signed S3 upload path (or managed storage SDK) for private bucket setups.
- Extend Brightway validator coverage to `solve_batch` and multi-impact (`impact_count > 1`) paths.
- Add compression telemetry fields (`raw_bytes`, `compressed_bytes`, `ratio`, `compress_sec`) to reports for sustained tuning.

### P2

- Optional L2 cache for warm startup.
- Add CHOLMOD/SPQR backends behind unified trait.
- Add richer post-processing (top contributors/path analysis).

## 7. Safe change rules

- Keep solver invariant: factorize once, solve many.
- Keep heavy compute off synchronous request paths.
- Keep C FFI behind safe Rust wrappers.
- Always validate sparse matrix structure before numeric factorization.
- Never introduce explicit matrix inverse.
- For schema migration work: additive-first unless explicitly approved.
- If behavior/contracts/status changes, update this file in the same change set.

## 8. Local runbook (agent)

Install system deps (Ubuntu):

```bash
sudo apt-get update
sudo apt-get install -y libsuitesparse-dev libopenblas-dev liblapack-dev pkg-config cmake
```

Run checks:

```bash
make check
```

Run worker:

```bash
set -a && source .env && set +a
cargo run -p solver-worker --release
```

Build one snapshot artifact:

```bash
./scripts/build_snapshot_from_ilcd.sh --process-limit 100
```

Run end-to-end debug (`prepare` + `solve_one`):

```bash
./scripts/run_full_compute_debug.sh --snapshot-id <snapshot_id>
```

Run benchmark-oriented debug (skip artifact encode/upload, keep inline result for validation):

```bash
./scripts/run_full_compute_debug.sh --snapshot-id <snapshot_id> --result-persist-mode inline-only
```

Run manual Brightway25 validation (default pipeline does not trigger this automatically):

```bash
./scripts/run_bw25_validation.sh --snapshot-id <snapshot_id>
```

Clean local generated artifacts:

```bash
./scripts/cleanup_local_artifacts.sh
./scripts/cleanup_local_artifacts.sh --dry-run
./scripts/cleanup_local_artifacts.sh --with-target
```

## 9. Definition of done

A task is complete only when:

- code compiles
- `fmt/clippy/test` pass
- worker queue flow still works
- docs stay in sync (`AGENTS.md` + human-facing `README.md`)
