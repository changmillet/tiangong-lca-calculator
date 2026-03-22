# TIDAS Export Reliability Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ensure async TIDAS export jobs always reach a terminal state and can successfully export full user-scoped data packages, even when the user owns large numbers of `processes` and `flows`.

**Architecture:** Replace the current one-shot export path with a staged, resumable worker workflow. The worker should persist export frontier state in PostgreSQL, materialize JSON files incrementally to a local workspace, and only finalize/upload the ZIP after all datasets have been collected. Keep the existing Edge Function request contract and Next.js polling contract, but extend job status responses with stage and progress fields.

**Tech Stack:** Rust (`solver-worker`), PostgreSQL + `pgmq`, Supabase Edge Functions, Next.js, Supabase Storage.

---

### Task 1: Persist Export Progress State

**Files:**
- Create: `supabase/migrations/<new>_tidas_export_resume_state.sql`
- Modify: `docs/tidas-package-contract.md`
- Modify: `README.md`

**Step 1: Add resumable job columns**

Extend `lca_package_jobs` with fields for:
- `stage text`
- `heartbeat_at timestamptz`
- `lease_expires_at timestamptz`
- `progress jsonb`

Use `stage` values:
- `queued`
- `seed_scan`
- `collect_refs`
- `materialize_files`
- `finalize_zip`
- `upload_artifacts`
- `ready`
- `failed`

**Step 2: Add export item state table**

Create `lca_package_export_items`:
- `job_id uuid not null`
- `table_name text not null`
- `dataset_id uuid not null`
- `version text not null`
- `is_seed boolean not null default false`
- `refs_done boolean not null default false`
- `file_done boolean not null default false`
- `created_at timestamptz not null default now()`
- `updated_at timestamptz not null default now()`

Add unique constraint:
- `(job_id, table_name, dataset_id, version)`

Add indexes:
- `(job_id, refs_done)`
- `(job_id, file_done)`
- `(job_id, is_seed)`

**Step 3: Validate migration shape**

Run:

```bash
./scripts/validate_additive_migration.sh supabase/migrations/<new>_tidas_export_resume_state.sql
```

Expected:
- `additive migration checks passed`

**Step 4: Document the new job state model**

Update:
- `docs/tidas-package-contract.md`
- `README.md`

Document:
- staged export lifecycle
- meaning of `stage`, `progress`, `heartbeat_at`
- distinction between seed roots and dependency entries

**Step 5: Commit**

```bash
git add supabase/migrations/<new>_tidas_export_resume_state.sql docs/tidas-package-contract.md README.md
git commit -m "feat: persist resumable tidas export state"
```

### Task 2: Refactor Worker into a Staged Export FSM

**Files:**
- Modify: `crates/solver-worker/src/package_db.rs`
- Modify: `crates/solver-worker/src/package_types.rs`
- Modify: `crates/solver-worker/src/bin/package_worker.rs`
- Create: `crates/solver-worker/src/package_export_state.rs`
- Modify: `crates/solver-worker/src/lib.rs`

**Step 1: Add typed export stage helpers**

Create a small state module that defines:
- `PackageExportStage`
- stage transition helpers
- heartbeat/progress payload builders
- lease validation helpers

**Step 2: Change one-shot execution to one-stage execution**

Today `crates/solver-worker/src/package_db.rs` around line 254 awaits the entire export path in one call. Replace that with:
- load current job stage
- execute only one batch of work for that stage
- persist updated `stage` + `progress`
- if unfinished, enqueue the same `job_id` again
- return so the current queue message can be archived

**Step 3: Add lease / heartbeat semantics**

Before processing a stage batch:
- acquire or renew a job lease
- set `heartbeat_at = now()`
- set `lease_expires_at = now() + interval`

On each successful batch:
- refresh `heartbeat_at`
- archive current queue message
- enqueue next batch if work remains

**Step 4: Make stuck jobs reclaimable**

If a worker sees:
- `status = running`
- `lease_expires_at < now()`

it may safely resume the same job instead of creating a new one.

**Step 5: Add focused tests**

Add tests for:
- transition from `queued -> seed_scan`
- stage re-enqueue when work remains
- reclaiming an expired lease
- failure transitions to `failed`

Run:

```bash
cargo test -p solver-worker package_db -- --nocapture
```

Expected:
- export state transition tests pass

**Step 6: Commit**

```bash
git add crates/solver-worker/src/package_db.rs crates/solver-worker/src/package_types.rs crates/solver-worker/src/bin/package_worker.rs crates/solver-worker/src/package_export_state.rs crates/solver-worker/src/lib.rs
git commit -m "feat: refactor tidas export into staged worker flow"
```

### Task 3: Collect Export Data Incrementally Instead of One-Shot

**Files:**
- Modify: `crates/solver-worker/src/package_execution.rs`
- Modify: `crates/solver-worker/src/package_db.rs`
- Create: `crates/solver-worker/tests/package_export_collection.rs`

**Step 1: Split seed discovery from dependency collection**

Replace the current one-shot call in `crates/solver-worker/src/package_execution.rs` around lines 183 and 412 with two explicit stages:
- `seed_scan`: insert user-scope or selected-root datasets into `lca_package_export_items`
- `collect_refs`: read a small batch of unresolved items, extract references, and upsert newly discovered dependencies

**Step 2: Keep seed roots separate from dependencies**

When `scope != selected_roots`, do not derive manifest roots from the full collected closure. Persist:
- `is_seed = true` for user/open-data scope datasets
- `is_seed = false` for recursively discovered dependencies

This fixes the current semantic leak where dependency datasets can appear as roots.

**Step 3: Batch the collection loop**

Use small batches, for example:
- `seed_scan`: 200 rows per table batch
- `collect_refs`: 100 unresolved items per worker turn

For each batch:
- fetch exact rows
- extract references
- bulk fetch referenced rows by table
- upsert newly discovered keys
- mark the current batch `refs_done = true`

**Step 4: Prefer correctness over clever dedup**

Do not attempt deep optimization in this pass. The success criterion is:
- every reachable dependency is eventually inserted once into `lca_package_export_items`
- the worker can stop and resume without losing state

**Step 5: Add tests covering the failure case we saw**

Create a fixture that simulates:
- many user-owned `processes`
- many user-owned `flows`
- references to shared/open datasets

Assert:
- all user-owned datasets become seeds
- referenced open/shared datasets are discovered
- repeated worker turns converge to a stable closure

Run:

```bash
cargo test -p solver-worker package_export_collection -- --nocapture
```

Expected:
- batched collection finishes and produces stable item counts

**Step 6: Commit**

```bash
git add crates/solver-worker/src/package_execution.rs crates/solver-worker/src/package_db.rs crates/solver-worker/tests/package_export_collection.rs
git commit -m "feat: collect tidas export data incrementally"
```

### Task 4: Materialize Files Incrementally and Stream ZIP Finalization

**Files:**
- Modify: `crates/solver-worker/src/package_execution.rs`
- Modify: `crates/solver-worker/src/package_artifacts.rs`
- Modify: `crates/solver-worker/src/storage.rs`
- Create: `crates/solver-worker/tests/package_export_zip.rs`

**Step 1: Introduce a per-job workspace**

Use a local workspace path such as:

```text
/tmp/tidas-package-export/<job_id>/
```

Layout:
- `manifest.json`
- `<table>/<id>_<version>.json`

**Step 2: Materialize dataset files in batches**

Add a `materialize_files` stage:
- fetch a batch of `file_done = false` items
- read exact row payloads
- write one JSON file per dataset
- mark `file_done = true`

**Step 3: Build ZIP from filesystem, not from a giant in-memory Vec**

Replace the current `build_package_zip()` call in `crates/solver-worker/src/package_execution.rs` around line 550 with:
- write manifest file
- stream workspace files into a ZIP on disk
- upload the resulting file

This change is not about speed first; it is about avoiding one huge in-memory ZIP build that can stall or fail invisibly.

**Step 4: Upload artifacts only after files exist**

Keep the current artifact contract, but only in the final stages:
- upload `export_zip`
- insert `export_zip` row
- generate/upload `export_report`
- insert `export_report` row
- mark job `ready`

**Step 5: Add ZIP contract tests**

Assert:
- one JSON per dataset
- grouped by table folder
- manifest lists only seeds in `roots`
- dependencies still exist in `entries`

Run:

```bash
cargo test -p solver-worker package_export_zip -- --nocapture
```

Expected:
- ZIP structure matches the package contract

**Step 6: Commit**

```bash
git add crates/solver-worker/src/package_execution.rs crates/solver-worker/src/package_artifacts.rs crates/solver-worker/src/storage.rs crates/solver-worker/tests/package_export_zip.rs
git commit -m "feat: materialize tidas export files incrementally"
```

### Task 5: Expose Progress Through Existing APIs

**Files:**
- Modify: `tiangong-lca-edge-functions/supabase/functions/_shared/tidas_package.ts`
- Modify: `tiangong-lca-edge-functions/supabase/functions/tidas_package_jobs/index.ts`
- Modify: `tiangong-lca-next/src/services/general/api.ts`
- Modify: `tiangong-lca-next/src/components/ExportTidasPackage/index.tsx`
- Modify: `tiangong-lca-next/src/components/ExportData/index.tsx`

**Step 1: Keep request contract stable**

Do not change:
- `export_tidas_package` request body
- `tidas_package_jobs` route shape

Only extend the response payload with:
- `stage`
- `progress`
- `heartbeat_at`

**Step 2: Update frontend polling types**

In `next`, read the new fields from job status so the UI can distinguish:
- still collecting
- writing files
- finalizing zip
- failed

This is optional for correctness, but valuable for operator confidence and for verifying that jobs are not silently stuck.

**Step 3: Add API and component tests**

Run:

```bash
npm run lint
npx jest tests/unit/components/ExportData.test.tsx --runInBand
```

Expected:
- type checks/lint pass
- export component tolerates richer status payload

**Step 4: Commit**

```bash
git add tiangong-lca-edge-functions/supabase/functions/_shared/tidas_package.ts tiangong-lca-edge-functions/supabase/functions/tidas_package_jobs/index.ts tiangong-lca-next/src/services/general/api.ts tiangong-lca-next/src/components/ExportTidasPackage/index.tsx tiangong-lca-next/src/components/ExportData/index.tsx
git commit -m "feat: expose tidas export stage progress"
```

### Task 6: Validate the Full Recovery Path

**Files:**
- Modify: `README.md`
- Modify: `tiangong-lca-edge-functions/README.md`
- Modify: `tiangong-lca-next/src/locales/zh-CN/component_tidasPackage.ts`
- Modify: `tiangong-lca-next/src/locales/en-US/component_tidasPackage.ts`

**Step 1: Run end-to-end validation**

Sequence:

```bash
# calculator
cargo check -p solver-worker --all-targets

# edge-functions
npm run lint
deno check --config supabase/functions/deno.json supabase/functions/_shared/tidas_package.ts supabase/functions/export_tidas_package/index.ts supabase/functions/import_tidas_package/index.ts supabase/functions/tidas_package_jobs/index.ts

# next
npm run lint
npm run tsc
```

Expected:
- all three repos validate

**Step 2: Manual export smoke**

Manual checks:
- trigger full current-user export
- verify job transitions across stages
- verify ZIP artifact exists
- verify ZIP contains table folders and one JSON per dataset
- restart worker in the middle of export once and confirm the job resumes

**Step 3: Document operator runbook**

Add notes for:
- how to inspect stage progress
- how to identify stale jobs
- how to safely retry a stuck export

**Step 4: Commit**

```bash
git add README.md tiangong-lca-edge-functions/README.md tiangong-lca-next/src/locales/zh-CN/component_tidasPackage.ts tiangong-lca-next/src/locales/en-US/component_tidasPackage.ts
git commit -m "docs: add tidas export recovery runbook"
```

### Repo Ownership Summary

- `tiangong-lca-calculator`: mandatory; the core fix lives here
- `tiangong-lca-edge-functions`: small API surface update only
- `tiangong-lca-next`: optional UX/progress visibility; not required to make export succeed

### Recommended Delivery Order

1. `calculator#1`: staged resumable export state + worker refactor
2. `edge-functions#1`: expose stage/progress in job status
3. `next#1`: optionally surface progress in UI

### Why This Solves the Current Failure

- The current export path is one giant critical section from job `running` to artifact upload.
- If collection stalls, the user sees a forever-`running` job and there is no durable intermediate state.
- The proposed design makes export:
  - checkpointed
  - resumable
  - visible by stage
  - able to finish even if the worker restarts mid-export
