---
title: Study Owned Worker Runtime Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when executing Study source acquisition, model preparation or frozen Run jobs
  - when qualifying native process, Storage, cancellation and completion behavior
whenToUpdate:
  - when Study private RPC, child, artifact or resource protocols change
  - when supported runtime qualification or recovery evidence changes
checkPaths:
  - crates/solver-worker/src/study/**
  - crates/solver-worker/src/bin/study_native.rs
  - crates/solver-worker/src/bin/study_runtime_preflight.rs
  - crates/solver-worker/src/bin/study_artifact_reconcile.rs
  - crates/solver-worker/src/queue.rs
  - crates/solver-worker/src/storage.rs
  - docs/agents/contracts/study-runtime-contract.md
lastReviewedAt: 2026-11-04
lastReviewedCommit: accb92f
lastReviewedNote: "Worker #366: reviewed separately gated method daemon/native budgets, exact source/context rechecks, purpose-bound completion and formal reconciliation; actual Database, actor/Storage/report and complete industry/professional qualification remain required."
related:
  - study-compiler-contract.md
  - ../../lca-api-contract.md
  - ../../edge-function-integration.md
  - ../repo-validation.md
---

# Study owned runtime

`study::runtime` dispatches source, preparation, Run and analysis kinds from the
existing calculator/solver `worker_jobs` loop: `study.source_acquire`,
`study.model_prepare`, `study.run`, `study.analysis`, `study.analysis_advanced`,
`study.method_assessment`.
Private payload schemas are exactly `<kind>.request.v1`; payloads contain
`schemaVersion`, `studyId` and exactly one `acquisitionId`, `preparationId` or
`runId` or `analysisId` or `assessmentId`. Method payloads have exactly schema,
Study and assessment IDs; old payloads still reject `assessmentId`. Actor claims, source documents, source-state assertions, artifact
locators, results and credentials are not accepted in job payloads.

Successful domain callbacks atomically commit the artifact/domain association
and canonical job terminal result. Worker must not write generic success again.
The generic result is `lca.study-job-receipt.v1`, containing only schema, Study,
resource type and resource ID; numeric/source payloads and storage paths never
enter that shared public projection. Safe closed failure categories replace raw
native stderr or private RPC data.

The [analysis contract](study-analysis-contract.md) specifies the separate
`lca.study-analysis-input.v1` service DTO, ordered frozen Run artifacts,
`svc_study_analysis_read_input_v1` / `svc_study_analysis_complete_v1` signatures,
canonical facts and comparison limits. It emits immutable `analysisFacts`,
using this same supervised native/upload/readback/fenced completion path.
Database must extend scoped task/failure/reconciliation/current-source rights
and generic-route guards before admission. Merely adding a dispatch kind does
not activate or qualify any deployment. Public API versioning stays Edge-owned.

## Authoritative inputs and export identity

The service-only functions `svc_study_source_acquisition_read_input_v1`,
`svc_study_model_preparation_read_input_v1`, `svc_study_run_read_input_v1` receive
resource ID, actual job ID and current lease token. Database owns actor/client,
resource, current permission and lease checks. These are not caller-provided
`authorized` or `leaseCurrent` booleans. Completion uses the exact Database-owned `svc_study_source_record_v1`,
`svc_study_model_preparation_complete_v1` and `svc_study_run_complete_v1`
functions. The source recorder deliberately does not follow an inferred
`source_acquisition_complete` naming pattern. All three additionally receive
completion JSON and an idempotency key stable within that attempt.

Source acquisition uses the new, explicitly versioned DB-authorized JSON
SourceSnapshot export, not a legacy TIDAS ZIP or formal data publication. Its
format is `lca.study-source-snapshot.v1`, owned jointly by the reviewed resolver
boundary and Worker artifact emitter. `runtime_contracts.rs` specifies exact DTOs.
A packet contains origin, issuer/license reference, DB observation time,
service-issued source authorization and raw `lca.study-source.v1` documents.
Raw source packets have empty methods/cutoffs/overrides and null Study binding.

The resolver captures exact LifecycleModel -> explicit Process -> Flow ->
reference FlowProperty -> UnitGroup revisions under actual row state/owner and
source/disclosure policy. No missing version selects latest; Result Process 120
and the reserved range never become generic numerical inputs. Capture is reused
on recovery; changed mutable drafts are not silently recaptured under the same
acquisition. Per-document hashes cover exact stored `json_ordered` JSON UTF-8 text (no JSONB conversion/fallback) under
`postgresql.json-text.utf8.v1`. Worker verifies issuer, bound origin, claimed
actor and complete unique per-document scope/hash correspondence before export.

Snapshot SHA covers actual serialized SourceSnapshot bytes, never the RPC
`{ok,data}` wrapper. The compiler SourceInput is assembled later with explicit
methods/cutoffs or Scenario bindings and has its own distinct exact-byte hash.
Neither replaces normalized model, frozen input or core calculation identities.

Study byte/RPC decoding uses its own exact finite-f64 codec. It walks borrowed
raw JSON tokens with an 8 MiB byte limit, 262144-node limit and 128-level depth
limit checked before recursion. Representable integers remain i64/u64; floats
use Rust's roundtrip parser. Duplicate/reserved Serde keys, malformed/deep input
and nonfinite/out-of-range values fail. Untyped allocation numbers use the same
bounded path. RPC JSON is fetched as exact text before this decoder, not first
rounded through SQLx's generic Value decoder. This prevents serialized decimal
matrices changing bits and breaking frozen semantic identities. Legacy global
float parsing and calculation descriptors remain unchanged. The raw_value API
was already enabled by existing SQLx/Axum; its explicit dependency declaration
adds no version or resolved feature. No semantic-hash check is bypassed.
Source time is parsed structurally; the host's wall clock does not decide DB
lease or capture eligibility.

Raw snapshot bytes contain service-scoped actor/owner authorization facts.
Study membership alone is not a grant to download them. Database/Edge must
apply the reviewed underlying source/disclosure policy or issue a separately
hashed approved projection; stripping fields cannot preserve the private
snapshot's byte hash. No JWT, session, access key or signing secret belongs in
these packets. Prepared models omit actor/owner IDs and raw source documents.

## Frozen preparation and execution

Private artifact wrappers carry exact Study, typed artifact reference, private
bucket and owned object path. Worker checks Study/artifact/purpose/size/schema
before every bounded download and verifies actual bytes against the issued hash.
Only `study-artifacts`, `study/<studyId>/<artifactId>/<simple-name>.json` is used.
No actor supplies a URL or bucket.

Base preparation takes ordered exact methods and the captured optional structured
cutoffs. Absent cutoffs means none: no exclusions or providers are fabricated.
Scenario preparation inherits cutoffs and the cumulative override layer from the
model selected by its exact frozen StudyRevision. Its current overrides are a
complete explicit layer relative to that model; a parent Scenario is lineage,
not another implicitly merged patch layer. Existing base overrides survive;
current layer values replace the same paths.

Scenario factors are reconstructed only from the verified frozen base C matrix,
method/inventory axes, coverage gaps and method-source identity. Missing factors
and known numeric zero remain distinct. There is no live static-factor or
background reload for Scenario preparation or Run. Exact source documents must
match the original frozen snapshot. Run consumes only its selected verified
prepared bytes, model/input identity and exact Study/Scenario bindings.

Native output is checked again for purpose, effective SourceInput hash, frozen
binding, expected model/input identity and result shape before upload. Native
errors use a closed failure envelope, not source JSON or raw stderr. Source
acquisition does not certify science, license a paper, qualify full LCA or turn
a task-authored test model into an existing published LifecycleModel.

## Process, clock and resource policy

Synchronous parsing, static preload, compilation and core solve run in
`study_native`. The parent passes a private file path; its environment is cleared
and native math thread counts are fixed to one. Source/cache configuration is
server-owned. No executable, shell snippet or output path is accepted from an
actor. Native child output remains in a private job/attempt workspace until
verified upload and fenced completion.

A 120-second **local monotonic whole-attempt** budget includes input RPC/I/O,
component readiness, native preload/work, upload, readback and callback attempts.
The initial heartbeat is bounded by that same deadline; it cannot hold a claimed attempt indefinitely before native work starts. Failure acknowledgements use the same deadline-aware exact retry protocol. No new callback is initiated after that budget; an exhausted attempt leaves terminal readback/recovery to the canonical job lifecycle. A callback already dispatched
when the client times out may have committed under a valid DB lease: its outcome
is unknown, not remotely aborted. Identical callback retries recover the
original receipt. Once any dispatched attempt is ambiguous, a later authorization or lease rejection cannot prove that the earlier attempt did not commit; the outcome stays unknown until success or scoped authoritative reconciliation. A first explicit rejection remains a known rejection. Unknown outcomes retain objects and ownership receipts and do
not trigger a contradictory generic terminal write. Accepted cancellation,
expired lease or takeover remains the atomic DB publication fence.

Periodic DB renewal is capped at two seconds even for long leases; the parent
never compares `leaseExpiresAt` to host UTC. A cancellation/lease-loss signal is
propagated to I/O and the child. Native child is killed and awaited before normal
failure cleanup. The job future is joined, not dropped on monitor failure.
Periodic renewal stops for the final fenced callback after an authoritative last
heartbeat, avoiding a completed job's expected renewal rejection being treated
as a competing terminal outcome.

The first profile uses W05's 4 MiB source/prepared/transfer ceiling, input/model
admission bounds, one claimed job per worker, native RSS threshold 512 MiB and
bounded file/request sizes. RSS is sampled every 100ms: Linux `/proc`, macOS
bounded `/bin/ps`, Windows `K32GetProcessMemoryInfo` with the documented
`PROCESS_MEMORY_COUNTERS` layout. Probe failure fails closed. Sampling and OS
scheduling allow overshoot; this is **not a kernel hard RSS limit**. Production
qualification must bind the actual existing container/task outer memory and
process supervision separately. The local native component receipt does not
claim that containment.

The platform matrix remains glibc Linux x64/arm64, macOS arm64, Windows MSVC x64.
Unsupported ABI/architecture fails explicitly; macOS Intel is not supported.
Every advertised native server tuple must execute `study_runtime_preflight` on
that actual host: linked core arithmetic, OS/ABI/component identity, successful
RSS observation and actual memory/deadline/cancel kill-and-reap checks. Reading
Windows API docs or cross-compiling is not Windows executable qualification.
Pure HTTP/CLI clients on Windows remain independent of the server host tuple.

## Artifact ownership, cleanup and recovery

Before PUT, a private atomic upload-intent receipt records exact Study/job/attempt,
artifact hash, object key and opaque storage-authority fingerprint. Original
short-lived lease token and scoped resource/idempotency identity remain only in
that private 0600 journal so its scoped reconciliation RPC can prove the attempt;
they never enter a locator, generic job payload/result or public error. Failure to
journal prevents upload. Object filenames include job and attempt; no lease token
is embedded in a locator. Upload and readback are bounded and hash verified.
The authority fingerprint excludes signing secrets/session tokens; it binds the
configured endpoint/region/bucket/access-key identity to prevent reconciliation
against an unrelated environment.

Known prepublication failures delete owned successful uploads; delete failure or
ambiguous upload/callback retains a receipt. `study_artifact_reconcile` calls the service-only
`svc_study_task_reconcile_v1` for canonical artifact attachment, resource/job
scope and DB-clock lease facts. Only its `unreferenced` decision permits deletion;
`committed`/`referenced` retain data and `pending` retains the receipt. Worker
uses `svc_study_task_fail_v1` for closed-code failure projection and never bypasses
the domain with a generic terminal writer. It never deletes a registered attached artifact. Unattached exact
objects are eligible only after the scoped RPC proves a durable terminal job,
no remaining publication lease/receipt and no artifact metadata. An expired or
reclaimed attempt while the job remains active is still pending. Their bytes
must match the ownership receipt. Age or a stopped
process alone proves nothing. A currently absent object from an ambiguous PUT
keeps its receipt rather than claiming an unknowable future request outcome.
Verified deletion receipts remain private cleanup evidence.

Journal entries are private and bounded (4096 entries); overflow blocks new
uploads. Source/temp roots must be private on Unix. Normal completion/failure
removes the private TempDir after child reaping. Abrupt parent-process death
cannot execute Rust destructors: job/attempt-prefixed workspaces contain
ownership metadata for controlled recovery, while production container/task
supervision owns the full process group. Do not claim arbitrary SIGKILL cleanup
from `kill_on_drop`. No cleanup command scans or deletes by age.

## Operational admission

The runtime is off unless `STUDY_RUNTIME_ENABLED=true`. Configure absolute
`STUDY_NATIVE_BIN`, private `STUDY_ORPHAN_RECEIPT_DIR` and `STUDY_TEMP_ROOT`, the
existing DB/queue/S3 settings and base-preparation-only trusted LCIA cache source.
Build/install the native child alongside the daemon; no PATH/cargo/source-tree
fallback is used. Set claim limit to one. Operators qualify matching deployed
workers, native assets, Storage and DB migrations before enabling registry/API
capabilities; an old calculator claimant must not consume newly enabled kinds.

Component-only proof and mocked boundaries are not the actual source-to-Run
qualification. Record separate evidence for local component, real actor API,
actual source resolver, actual Storage, native process, canonical lease/terminal
callbacks and deployment. Root coordination retains exact source/build/receipt
identities; no hosted deployment follows from source merge or this document.

## W14 advanced native boundary

The private `analyze_advanced` action consumes `lca.study-analysis-native.v2`,
with exact frozen baseline/source files and explicit experimental BatchLimits.
Its closed config/facts/input v2 formats and numerical interpretation live in
[the analysis contract](study-analysis-contract.md). Aggregate native timing and
all-or-error diagnostics do not relax the existing parent attempt, lease,
cancellation, source rights, journal, upload or unknown-callback invariants.
There is no implied daemon/API admission from that native component.


### Explicit advanced daemon admission

The separate advanced job now uses the agreed Database #909 / Edge #553
`svc_study_analysis_advanced_read_input_v1` and
`svc_study_analysis_advanced_complete_v1` seams. The read DTO is input.v2;
completion is facts.v2 without W08 comparability. The parent binds exact
Study/analysis/job, downloads the original source snapshot and baseline
prepared/result through their issued private capabilities, and sends only
those verified files to `analyze_advanced`. No live source or factor reload
is introduced. After native execution, the parent validates the complete
facts catalog, frozen identities and effective limits before the existing
journal/upload/readback/fenced completion protocol.

Admission additionally requires `STUDY_ADVANCED_ENABLED=true`, explicit positive
`STUDY_ADVANCED_MAX_PARAMETERS` and `STUDY_ADVANCED_MAX_EVALUATIONS` (at least
three, including the baseline reproduction). Missing or malformed values reject
only advanced jobs; deterministic handlers retain their admission contract.
The service/operator must bind these limits to actual qualification evidence;
no default advanced capacity is advertised. The DB configuration independently
limits public admission. The existing whole-attempt deadline, native aggregate
budget, artifact/RSS limits, single claim, cancellation and unknown-outcome
retention remain unchanged. Code wiring and local fixture tests do not qualify
the actual installed SQL/Storage/actor pipeline or a hosted deployment.


## Method-assessment daemon boundary

The separate default-off method job expects the frozen interfaces in the
[method contract](study-method-assessment-contract.md). Native action
`assess_method` consumes private native.v1 and the distinct job-context.v1
wrapper without widening its inner assessment-input.v1. The parent binds
Study/assessment/job and admitted selected-factor ceiling to its independently
configured `STUDY_METHOD_MAX_SELECTED_FACTOR_RECORDS`; `STUDY_METHOD_ENABLED=true`
and a positive ceiling at most8192 are both required. Missing settings reject
only method jobs. New job admission remains independently Database-owned.

The Worker downloads the three exact SourceSnapshot/prepared/result private
artifacts, then resolves the same current context again before native execution.
Pinned method loading and frozen-source re-preparation run inside the existing
supervised native process, with the selected-factor bound checked before source
expansion. These verification steps create no new deterministic Run or source
snapshot and do not replace the original frozen method/calculation identities.
The whole-attempt deadline/RSS/cancellation/kill-and-reap policy remains in force.

The parent verifies registered facts purpose, all25 category identities, all20
requirement/professional states, exact scalar catalog, N/W availability and
original baseline references. It reauthorizes through input resolution again
before upload. Method completion carries actual selected-factor record count
(including missing-factor records) and exact immutable facts ref. Only this
family uses its distinct read/complete/fail/reconcile RPCs. A completion success
must return the exact method record/job/config/artifact; malformed success is
ambiguous and retains the original journal through the sticky-unknown retry
protocol. Formal reconciliation checks the exact completed record before
retiring a method upload journal. No actor/source permission is renewed by it.

Local source/fixture/native proof does not establish actual Database issuance,
OAuth/Storage/report consumers, supported-host method capacity or a complete
industry positive. Those remain independently required through the tracked
cross-owner cohort. No migration or source merge enables this capability.
