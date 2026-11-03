---
title: Study Deterministic Analysis Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when producing or consuming frozen Study numerical facts and comparison evidence
  - when implementing the study.analysis private job or reporting boundary
whenToUpdate:
  - when analysis schemas, configuration, numeric spelling, attribution or comparison rules change
  - when analysis runtime bounds or qualification evidence change
checkPaths:
  - crates/solver-worker/src/study/analysis*
  - crates/solver-worker/src/study/analysis/**
  - crates/solver-worker/src/study/runtime*.rs
  - crates/solver-worker/tests/fixtures/study-analysis/**
  - docs/agents/contracts/study-analysis*.schema.json
lastReviewedAt: 2026-11-03
lastReviewedCommit: e7e416c2fc61da9520556077860823ffccecf742
lastReviewedNote: "Worker #360: W08 frozen-result analysis, separate config/facts codec and fenced native job; no P1 numerical/hash or publication-policy change."
related:
  - study-runtime-contract.md
  - study-compiler-contract.md
  - ../repo-validation.md
  - ../../lca-api-contract.md
---

# Study analysis

`lca.study-analysis-explicit-v1` produces immutable numerical facts from issued
P1 prepared inputs and results. It does not fetch latest datasets/factors,
recompile a model, change a Run, certify science, make a material/process
recommendation, authorize source disclosure or issue a report/export signature.
The existing calculation descriptor and P1 model/input/result encodings are
unchanged. Edge owns public request versions; Worker does not implement v3 actor
dispatch, membership or report persistence.

## Closed configuration and identities

`study-analysis-config.v1.schema.json` defines exactly:

```json
{"schemaVersion":"lca.study-analysis-config.v1","kind":"comparison","runIds":["<baseline UUID>","<alternative UUID>"]}
```

`contribution` requires one Run; `comparison` requires two distinct, ordered
Runs. The IDs are canonical lowercase UUIDs. There are no caller-controlled
method substitutes, source reloads, ranking authorization, truncation or
calculation thresholds. All frozen methods participate. Unsupported fields,
schemas, profiles and counts fail rather than being silently normalized.

`configHash = SHA256(UTF8("study.analysis-config.v1") + NUL + canonicalJSON(config))`.
The existing Worker semantic encoder sorts object keys lexicographically,
preserves array order and uses compact literal-Unicode UTF-8 with no newline.
This config contains fixed ASCII keys, enum strings and UUID strings, so Node
can implement the same bytes without floating-point conversion. The normative
canonical bytes/hash example is in `tests/fixtures/study-analysis/service-seams.json`.
The new config identity is not a P1 model hash, input hash, artifact-byte hash or
calculation fingerprint. Facts artifact hashes cover their exact emitted bytes.

## Exact input and result verification

`analysis_contracts.rs` is the producer DTO; the service fixture names exact
RPC arguments and complete example shapes. The private input is
`lca.study-analysis-input.v1`, with an analysis/job/config record and ordered
`runs:[{run,revision,scenario,preparation,preparedArtifact,resultArtifact}]`.
The four records reuse the unchanged Edge-owned P1 record contracts. The two
artifacts use the existing private Study wrapper and must be registered exact
`modelInput/lca.study-prepared-input.v1` and `runResult/lca.study-result.v1` refs.

Worker checks current-domain Study/Run/revision/Scenario/preparation bindings,
completed and frozen states, the preparation's original Scenario generation/hash,
byte sizes/hashes, model/input/calculation identities, compiler/profile, frozen
definition and ordered method axes, functional unit/reference demand, view and
inventory units, coverage, quality flags and result vector dimensions. It also
checks `(I-A)x=y`, `Bx=g`, `Cg=h` consistency. Source grants and lease authority
are independently enforced by Database; metadata/validity are not permissions.
No caller-provided `authorized`, `leaseCurrent` or successful result is accepted.

## Numerical meanings

Each activity fact is a frozen model-view/reference-flow quantity, not an
invented dimensionless multiplier. Inventory facts use exact Flow revision,
direction, unit and support identities. LCIA facts use the exact Method revision
and that Method's result-quantity label, not the generic mass unit.

Direct contribution of view `j` to method `m` is
`sum_i ((B[i,j] * x[j]) * C[m,i])`. This computes the inventory product before
characterization, matching P1's order; `x*sum(B*C)` can erase a representable
result through premature underflow. Intermediate non-finite or nonzero-to-zero
range loss fails explicitly. Zero coefficients/activity remain real zero.
Contribution totals are checked against the frozen `h` with a `1e-12` bound
scaled by absolute contribution magnitude; no fixed absolute epsilon discards
small values. A view is identified by instance/exchange port, even when multiple
instances share one Process. This is direct attribution of a simultaneous solve,
not repeated path traversal or upstream cumulative scores that double count cycles.

Inventory contribution is `C[m,i]*g[i]` only where that factor is matched.
Missing coverage stays explicit: unknown terms have null values, never invented
zero. Known zero factors remain numeric zero. Known LCIA terms in a partial
category retain `coverage:partial`; that is not a complete category result.
All-unknown categories emit null instead of presenting an empty numeric
projection as a measured zero. Activities/inventory are complete only within
their explicitly declared numerical scope, not a complete scientific inventory.

Hotspots list all view-contribution fact IDs by descending absolute characterized
contribution, with fact ID as the stable tie-break. They never drop nonzero
values, imply complete characterization or rank alternative technologies.

Comparison checks exact functional unit, boundary, ordered method axes/units,
calculation profile, method source, assumptions and cutoffs. A mismatch is
`not_comparable`, with undefined deltas. Matched numerical prerequisites with
coverage gaps, quality findings, noncurrent validity annotations or changed
source identities are `not_assessed`; clearly scoped characterized deltas may
still be returned. Otherwise `comparable` means only
`scope:frozen_numerical_inputs_only`. Equal revision IDs are not a scientific
equivalence assessment. This profile always emits `rankingAllowed:false`,
`scientificRankingQualified:false`, `businessPilotQualified:false`.

Absolute delta is alternative minus baseline. Relative delta is explicitly
`delta / abs(baseline)`, dimensionless `unit:"1"`, with Method ref retained for
category provenance. A zero baseline is undefined, not zero percent. The stored
scope makes this denominator unambiguous for negative baselines. No unit
conversion or automatic improvement/superiority interpretation is performed.

## Facts and portable numeric strings

`study-analysis-facts.v1.schema.json` defines the strict public artifact. Its
`inputs` retain exact Run/model/prepared/result/calculation refs and original
validity annotations. `coverage` and `scenarioEvidence` retain gaps, quality
findings and frozen Scenario changes. They contain no storage bucket/path,
source actor/owner authorization, credential or raw source documents.
Unit identities are bound separately from display labels. `methodRef:null` is
required for activity/inventory; a dimensionless relative delta may have a
Method ref for provenance without borrowing that Method's physical unit.

Each fact ID is scoped by the exact bundle artifact identity; an ID alone is
not a global reference. Reports must bind both artifact and fact ID, exact Run
context, canonical value, unit identity and display rules. All tables, charts
and structured numeric prose must use that same resolved fact. These facts do
not validate arbitrary unstructured interpretation or grant report approval.

Canonical scientific values are **strings**. Producer spelling is pinned
serde_json shortest-roundtrip finite binary64, followed only in new facts by:

1. Both signed zeros become `"0"`.
2. Fractional trailing zeros and an empty decimal point are removed.
3. Lowercase `e`; remove exponent `+` and leading zeros; omit exponent zero.

Examples: `1.0→"1"`, `-0.0→"0"`, `1e+20→"1e20"`, minimum subnormal
`"5e-324"`. Strings are at most64 bytes and must roundtrip to the same nonzero
f64 bits. Scenario evidence override values use the same spelling; the original
P1 Scenario content hash remains unchanged. Integers such as indexes and byte
sizes are metadata, not scientific values. Consumers must not regenerate
canonical values through JS Number or compute contributions/comparisons.
Display rounding is separately owned by the reporting/view protocol.

The JSON Schemas express cardinality and known/unknown value constraints as
mutually exclusive strict `oneOf` objects for the pinned Node converter. Numeric
regex alone is not proof of shortest-roundtrip spelling or trusted issuance.

## Private job and completion

The existing solver `worker_jobs` path adds `study.analysis`, payload schema
`study.analysis.request.v1`, containing only `schemaVersion`, `studyId`,
`analysisId`. Read input uses exactly
`api.svc_study_analysis_read_input_v1(p_analysis_id uuid,p_job_id uuid,p_lease_token uuid)`.
Completion is exactly
`api.svc_study_analysis_complete_v1(p_analysis_id uuid,p_job_id uuid,p_lease_token uuid,p_completion jsonb,p_idempotency_key text)`.
Both are service-only. Completion contains `profileId`, `configHash`,
`comparability:{status,reasonCodes}`, issued `factsRef` plus internal storage
bucket/path. Database atomically installs that immutable association and the
locator-free `lca.study-job-receipt.v1` with resourceType `analysis`.

Database must recheck original actor/client, live Study/source scope and exact
lease/job/domain before read and completion, fence cancel/takeover, and extend
scoped task/failure/reconciliation/disclosure helpers and generic-route denial.
Each analysis/report disclosure must cover all referenced Runs' captured source
rights. Edge must retain pre/post-I/O authorization and byte integrity checks;
Study membership alone is not a source grant. A trusted Worker callback does not
authorize a future download or export. Report/export production belongs to W09.

The native branch reuses the existing kill-and-reap supervisor, 120-second
whole-attempt monotonic budget, heartbeat and unknown-outcome ownership journal.
Native analysis has 30 seconds, an aggregate input-byte owned-memory estimate,
at most8192 facts and a 4MiB artifact ceiling. Outputs are all-or-error, never
silently truncated. These independent bounds do not promise that every maximum
P1 model fits a maximum-detail analysis. RSS remains a sampled threshold, not
hard cgroup containment or a production SLA. Qualification and actual owned
runtime commands are listed in the validation guide.
