# TIDAS Package Worker Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Move full TIDAS ZIP export/import off synchronous edge functions and onto an async worker + artifact pipeline that can handle large packages safely.

**Architecture:** Keep the current split where edge functions authenticate, validate, and enqueue work, but add a package-specific async path in `tiangong-lca-calculator`. Use dedicated package job/artifact/cache tables and a dedicated queue so package traffic does not overload or overfit the existing `lca_jobs` snapshot-solver contract.

**Tech Stack:** PostgreSQL + `pgmq`, Rust (`solver-worker` workspace), object storage artifacts, Supabase Edge Functions, Next.js frontend polling/download UX.

---

### Task 1: Add package job schema and queue

**Files:**
- Create: `tiangong-lca-calculator/supabase/migrations/20260319120000_tidas_package_job_tables.sql`
- Modify: `tiangong-lca-calculator/README.md`
- Test: `tiangong-lca-calculator/supabase/migrations/20260319120000_tidas_package_job_tables.sql`

**Step 1: Write the migration**

Add additive-only schema for:
- `lca_package_jobs`
- `lca_package_artifacts`
- `lca_package_request_cache`
- `pgmq` queue `lca_package_jobs`

Include:
- `job_type` values: `export_package`, `import_package`
- `status` values: `queued`, `running`, `ready`, `completed`, `failed`, `stale`
- ownership fields: `requested_by`, `scope`, `root_count`
- storage fields: `source_artifact_url`, `result_artifact_url`, `report_artifact_url`
- request idempotency: `request_key`, `idempotency_key`

**Step 2: Validate the migration is additive**

Run: `./scripts/validate_additive_migration.sh supabase/migrations/20260319120000_tidas_package_job_tables.sql`
Expected: script exits `0`

**Step 3: Document the new migration**

Add one `README.md` entry describing the new package-worker schema and queue.

**Step 4: Commit**

```bash
git add supabase/migrations/20260319120000_tidas_package_job_tables.sql README.md
git commit -m "feat: add package job schema"
```

### Task 2: Define package payload and artifact contracts

**Files:**
- Create: `tiangong-lca-calculator/crates/solver-worker/src/package_types.rs`
- Create: `tiangong-lca-calculator/docs/tidas-package-contract.md`
- Modify: `tiangong-lca-calculator/crates/solver-worker/src/lib.rs`
- Test: `tiangong-lca-calculator/crates/solver-worker/src/package_types.rs`

**Step 1: Write the failing tests**

Add tests for:
- `export_package` payload deserialization
- `import_package` payload deserialization
- artifact manifest metadata serialization

**Step 2: Run the focused tests**

Run: `cargo test -p solver-worker package_types -- --nocapture`
Expected: FAIL because the module does not exist yet

**Step 3: Write minimal implementation**

Define:
- `PackageJobPayload`
- `PackageExportScope`
- `PackageRootRef`
- `PackageImportSource`
- `PackageArtifactKind`
- artifact/report metadata structs

Keep these types package-specific instead of extending `JobPayload`.

**Step 4: Re-run the focused tests**

Run: `cargo test -p solver-worker package_types -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/package_types.rs crates/solver-worker/src/lib.rs docs/tidas-package-contract.md
git commit -m "feat: add package payload contract"
```

### Task 3: Add package artifact encode/decode support

**Files:**
- Create: `tiangong-lca-calculator/crates/solver-worker/src/package_artifacts.rs`
- Modify: `tiangong-lca-calculator/crates/solver-worker/src/lib.rs`
- Test: `tiangong-lca-calculator/crates/solver-worker/src/package_artifacts.rs`

**Step 1: Write the failing tests**

Cover:
- export result manifest artifact encoding
- import report artifact encoding
- deterministic checksum generation

**Step 2: Run the tests**

Run: `cargo test -p solver-worker package_artifacts -- --nocapture`
Expected: FAIL because the module does not exist yet

**Step 3: Implement the minimal artifact helpers**

Use JSON artifacts first for:
- export manifest/report metadata
- import conflict report

Leave the final ZIP byte generation to the package worker execution path, but define artifact format identifiers now.

**Step 4: Re-run the tests**

Run: `cargo test -p solver-worker package_artifacts -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/package_artifacts.rs crates/solver-worker/src/lib.rs
git commit -m "feat: add package artifact helpers"
```

### Task 4: Implement package job DB helpers and queue consumer

**Files:**
- Create: `tiangong-lca-calculator/crates/solver-worker/src/package_db.rs`
- Create: `tiangong-lca-calculator/crates/solver-worker/src/bin/package_worker.rs`
- Modify: `tiangong-lca-calculator/crates/solver-worker/src/lib.rs`
- Modify: `tiangong-lca-calculator/crates/solver-worker/Cargo.toml`
- Test: `tiangong-lca-calculator/crates/solver-worker/src/package_db.rs`

**Step 1: Write the failing tests**

Add unit tests for:
- parsing package queue payloads
- extracting `job_id`
- formatting DB write diagnostics

**Step 2: Run the focused tests**

Run: `cargo test -p solver-worker package_db -- --nocapture`
Expected: FAIL because the module and binary support do not exist yet

**Step 3: Implement minimal queue and DB plumbing**

Add helpers for:
- reading `lca_package_jobs` queue messages
- updating `lca_package_jobs.status`
- inserting `lca_package_artifacts`
- marking `lca_package_request_cache` running/ready/failed

Create a dedicated `package_worker` binary that reuses shared config and object storage client, but consumes `lca_package_jobs`.

**Step 4: Re-run the focused tests**

Run: `cargo test -p solver-worker package_db -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/package_db.rs crates/solver-worker/src/bin/package_worker.rs crates/solver-worker/src/lib.rs crates/solver-worker/Cargo.toml
git commit -m "feat: add package worker queue plumbing"
```

### Task 5: Wire export/import execution and validate end-to-end contract

**Files:**
- Create: `tiangong-lca-calculator/crates/solver-worker/src/package_execution.rs`
- Modify: `tiangong-lca-calculator/crates/solver-worker/src/package_db.rs`
- Modify: `tiangong-lca-calculator/docs/tidas-package-contract.md`
- Test: `tiangong-lca-calculator/crates/solver-worker/src/package_execution.rs`

**Step 1: Write the failing tests**

Cover:
- export job result metadata generation
- import conflict summary generation
- open-data filter vs user-conflict rejection semantics

**Step 2: Run the focused tests**

Run: `cargo test -p solver-worker package_execution -- --nocapture`
Expected: FAIL because execution module does not exist yet

**Step 3: Implement the minimal execution shell**

In this task:
- read source artifact / request metadata
- reserve target artifact keys
- emit structured diagnostics and reports

If ZIP generation and JSON traversal are still too large for one commit, stub the execution boundary cleanly and land contract-complete scaffolding first.

**Step 4: Run validation**

Run:
- `cargo fmt --check`
- `cargo clippy -p solver-worker --all-targets --all-features -- -D warnings`
- `cargo test -p solver-worker`

Expected: PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/package_execution.rs crates/solver-worker/src/package_db.rs docs/tidas-package-contract.md
git commit -m "feat: scaffold package worker execution path"
```

### Task 6: Cross-repo handoff after calculator foundation lands

**Files:**
- Modify later: `tiangong-lca-edge-functions/README.md`
- Modify later: `tiangong-lca-edge-functions/supabase/functions/...`
- Modify later: `tiangong-lca-next/src/services/general/api.ts`
- Modify later: `tiangong-lca-next/src/components/...`

**Step 1: Edge handoff**

Define edge API around:
- create export job
- create import job
- get job status
- get signed download URL
- get signed upload URL

**Step 2: Frontend handoff**

Define next flow around:
- submit
- poll
- ready/failure state
- artifact download
- import report rendering

**Step 3: Workspace integration**

After submodule PRs merge, update workspace submodule pointers in `lca-workspace`.
