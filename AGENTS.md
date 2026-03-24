# AGENTS.md

This document is for AI coding agents working in this repository.

## 0. Mandatory maintenance rule

`AGENTS.md` is a living contract.
Every time code/schema/runtime behavior changes in this repo, update this file in the same task.

After every code change, run Clippy and make it pass before finishing the task.
This is a hard gate.

Required command:

```bash
cargo clippy -p solver-worker --all-targets --all-features -- -D warnings
```

After every code change, run Rust format check and make it pass before finishing the task.
This is a hard gate.

Required command:

```bash
cargo fmt --all -- --check
```

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
  - `solve_all_unit` (worker-internal chunked unit demand across all processes)
  - `invalidate_factorization`
  - `rebuild_factorization`
- updates `lca_jobs` status/diagnostics
- writes `lca_results` rows (artifact metadata only)
- updates request cache state in `lca_result_cache` by `job_id`

Package export/import worker path:

- binary: `crates/solver-worker/src/bin/package_worker.rs`
- consumes `pgmq` queue `lca_package_jobs`
- executes:
  - `export_package`
  - `import_package`
- updates `lca_package_jobs` status/diagnostics
- writes `lca_package_artifacts` rows for export ZIP / reports
- updates request cache state in `lca_package_request_cache`
- persists resumable export traversal state in `lca_package_export_items`
- export ZIP finalization now writes the package to a local temp file first, then uploads the final `export_zip` artifact via multipart S3 upload once the file exceeds the large-object threshold
- package job failure diagnostics for object-storage uploads now record structured fields such as `error_code`, `stage`, `upload_mode`, `artifact_byte_size`, `http_status`, and `storage_error_code`, with oversize uploads normalized to `artifact_too_large`
- package export `open_data` scope treats published rows as `state_code` `100..=199`
- full-scope export seed scanning currently:
  - rehydrates traversal cache from `lca_package_export_items` seed/external rows
  - scans source datasets with DB-side `jsonb_path_query_array(...)` extraction so the worker fetches reference payloads instead of full `json_ordered` blobs where possible
  - casts `json_ordered` / `json_tg` to `jsonb` inside the seed-scan query because source columns are not uniformly stored as `jsonb`

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
- same-source snapshot reuse is canonical-only:
  - when reuse hits an existing ready snapshot, the builder now resolves to the reused canonical `snapshot_id`
  - the builder must not persist alias snapshot rows that point a new `snapshot_id` at an older artifact URL
  - `build_snapshot` jobs must update `lca_jobs.snapshot_id` and `lca_active_snapshots.snapshot_id` to that resolved canonical id before completion
- process selection supports:
  - `--process-states <csv|all>` for `state_code` filtering (default runtime scope: `100..=199`)
  - `--include-user-id <uuid>` to include one user's processes in addition to `process-states` (OR union)
  - `--root-process <uuid@version>` to switch from broad filtered-library selection to request-scoped closure selection
  - request-root mode resolves the reachable public+private closure from explicit roots under the current `provider_rule`
  - request-root mode persists `selection_mode`, `request_roots`, `scope_hash`, and resolved-scope public/private counts into snapshot metadata
  - request-root mode does not allow `--process-limit`, because truncation would break closure semantics
- provider matching supports:
  - `strict_unique_provider` (legacy strict behavior)
  - `best_provider_strict` (auto-link select one provider by geo+time score)
  - `split_by_evidence` (strict weighted split by geo+time score)
  - `split_by_evidence_hybrid` (weighted split, fallback to equal split)
  - `split_equal` (always equal split for multi-provider)
- quantitative reference normalization is applied at build time:
  - mode: `reference_normalization_mode=strict|lenient` (CLI, default `strict`)
  - strict: missing/invalid `referenceToReferenceFlow` or reference amount => fail snapshot build
  - lenient: fallback scale `1.0` and record diagnostics
- allocation fraction is applied at exchange level:
  - source: `exchanges.exchange.allocations.allocation.@allocatedFraction`
  - mode: `allocation_fraction_mode=strict|lenient` (CLI, default `strict`)
  - strict: missing/invalid fraction => fail snapshot build
  - lenient: fallback fraction `1.0` and record diagnostics
- biosphere sign convention for elementary flows uses gross mode:
  - `Input`/`Output` no longer force direction sign flip; both are written with original `amount` sign into `B`
  - snapshot config persists `biosphere_sign_mode` (current runtime value: `gross`; legacy artifact default: `signed`)
- auto-link scoring currently uses only:
  - model priority pre-filter: prefer providers with the same `processes.model_id` as consumer when available
  - geography (`@location`) from process geography block
  - reference year (`common:referenceYear`) from process time block
- process source summary in request-root mode is now based on the resolved closure, not the entire broad candidate process scope
- snapshot coverage now includes additional matching diagnostics:
  - `matched_multi_resolved`
  - `matched_multi_unresolved`
  - `matched_multi_fallback_equal`
  - `a_input_edges_written`
- snapshot coverage also includes data-quality diagnostics:
  - `reference`: `process_total`, `normalized_process_count`, `missing_reference_count`, `invalid_reference_count`
  - `allocation`: `exchange_total`, `allocation_fraction_present_pct`, `allocation_fraction_missing_count`, `allocation_fraction_invalid_count`

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
- `20260308104000_lca_jobs_add_solve_all_unit.sql`
- `20260319120000_tidas_package_job_tables.sql`
- `20260319143000_tidas_package_export_items.sql`

Current runtime tables:

- `lca_network_snapshots`
- `lca_snapshot_artifacts`
- `lca_jobs`
- `lca_results`
- `lca_active_snapshots`
- `lca_result_cache`
- `lca_factorization_registry` (schema ready, runtime usage limited)
- `lca_package_jobs`
- `lca_package_artifacts`
- `lca_package_request_cache`
- `lca_package_export_items`

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

Required runtime for package import validation:

- `python3` available in worker runtime PATH
- Python module `tidas_tools` installed (must support `python3 -m tidas_tools.validate --input-dir <dir> --format json`)
- optional override: `TIDAS_VALIDATE_BIN` (custom validator command)

Note:

- Worker startup is expected to fail fast if required S3 config is missing.
- package `import_package` job now executes validator-before-import:
  - unzip source artifact to temp dir
  - run structured validator JSON report
  - if `summary.error_count > 0`, return import report `VALIDATION_FAILED` and skip both conflict checks and DB inserts
  - import report payload always includes validation counters/details (`summary.validation_issue_count/error_count/warning_count` + `validation_issues`)

## 8. Operations runbook

Build/check:

```bash
make check
```

Export latest matrices (A/B/M + one-impact C/H from latest `solve_all_unit`):

```bash
./scripts/export_latest_matrices.sh --impact-id <impact_uuid>
```

Mandatory lint gate after every code edit:

```bash
cargo clippy -p solver-worker --all-targets --all-features -- -D warnings
```

Mandatory format gate after every code edit:

```bash
cargo fmt --all -- --check
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
