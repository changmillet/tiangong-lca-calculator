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
  - crates/solver-worker/src/study/analysis_advanced/**
  - crates/solver-worker/src/study/runtime*.rs
  - crates/solver-worker/tests/fixtures/study-analysis/**
  - docs/agents/contracts/study-analysis*.schema.json
lastReviewedAt: 2026-11-04
lastReviewedCommit: 3d3159d9f231143a51f761f895fbd3aef18d1b9b
lastReviewedNote: "Worker #366: reviewed independent method-assessment library/offline candidate, exact source/frozen-byte checks and pending professional evidence; existing Run/runtime/API/consumer authority remains unchanged."
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

## W14 independent advanced profiles (not operationally admitted)

The separate `lca.study-analysis-config.v2`, `lca.study-analysis-input.v2` and
`lca.study-analysis-facts.v2` schemas and `analysis_advanced_contracts.rs` own
`lca.study-sensitivity-oat.v1` and `lca.study-uncertainty-bounded.v1`. They do not
change any W08 v1 bytes, contribution/comparison meaning or P1 identity.
`analyze_advanced` is a distinct private native action. Source merge, native
component proof and experimental limits do not enable a hosted job/profile.

Config is a closed `kind:sensitivityOat|uncertainty` object binding one exact
`baselineRunId` and an ordered, unique list of `{path,unit,baselineAmount,low,high}`.
Paths are eligible nonreference exchange amounts. `low < high` and
`low <= baselineAmount <= high` are required; width must be representable.
Each unit string is an assertion against the actual frozen exchange, not a unit
conversion or an authoritative identity. Facts retain the resolved exact Flow,
direction and FlowProperty/UnitGroup support revisions separately in
`resolvedParameters`; Method ID/version and method-source identity bind the
result quantity. No arbitrary matrix edits, changed providers, reference-amount
edits or evaluated expressions are accepted.

Every scientific config value is a **canonical string using the v1 facts codec**
above. Worker parses it as IEEE-754 binary64 with correctly rounded
round-to-nearest/ties-to-even conversion and requires the exact codec spelling
of that parsed value. Thus `"9.0"`, `"-0"`, overflowing/underflowing decimals
and decimals that round to a different canonical spelling are rejected.
Edge/SQL preserve and hash these strings; they must not regenerate them through
floating point. Canonical object JSON uses the existing semantic encoder;
`configHash=SHA256(UTF8("study.analysis-config.v2") + NUL + canonicalJSON(config))`.
Sample-count metadata is restricted to the exactly portable u32 integer domain;
this lexical/identity constraint is not an admitted execution ceiling.
The cross-owner hash example is `advanced-config-vectors.json`. Config serialized
bytes must fit the existing 262144-byte Edge request envelope; a complete request
including other fields must independently fit that transport cap. Sample count
and supplied empirical row count are separate resource axes.

Private input contains the unchanged W08 baseline Run/revision/Scenario/
preparation/artifact projections plus the original issued `sourceSnapshot` and
private `sourceArtifact` wrapper. All baseline checks, current actor/client/source
rights and exact lease/job/resource fences still apply. A wrapper or source
`sourceAuthorization` field is not itself a grant. Worker hash/size-verifies
exact snapshot, prepared and result bytes, verifies original source membership,
uses `VerifiedMethods::from_frozen`, inherits cutoffs and cumulative overrides,
and recompiles/replays the effective baseline before any evaluation. Each sample
starts again from that same frozen source and baseline override layer. There is
no live source/factor lookup. Evaluation identifiers are analysis-local
`baseline` and `evaluation-0...`; none is a Run or Scenario UUID. Baseline files
and records remain unchanged.

### OAT interpretation

One baseline replay and exactly two independent evaluations per ordered
parameter are compiled/solved: low then high. Other effective amounts remain at
the frozen baseline. Every Method output retains exact value, unit and coverage;
no-characterized-term categories are unknown/null rather than measured zero.
Absolute delta is endpoint minus baseline. Relative delta is
`delta / abs(baseline)`, dimensionless, and undefined for zero or unknown
baseline. Secant is `(highOutput-lowOutput)/(highAmount-lowAmount)` with separate
`outputUnit` and `inputUnit`; it is range dependent and is neither a local
derivative nor an interaction/global-sensitivity measure. Partial method
coverage remains partial on effects and statistics.

### Fixed-budget uncertainty and reproducibility

`independentUniform` requires `independenceDeclared:true` and draws parameters
in config order. `jointEmpirical` stores a nonempty rectangular table in that
same parameter order, verifies every amount against its declared bounds, and
samples an equal-weight **whole row with replacement** once per sample. It
preserves exactly the supplied row dependence, never an arbitrary correlation
matrix or an inference about source uncertainty. Repeated/constant rows retain
their declared weight. Each design has one fixed `sampleCount >= 2`; baseline is
not an extra stochastic sample and is excluded from summary statistics.

The pinned generator is `study.uncertainty.hmac-drbg-sha256.v1`:

- HMAC_DRBG SHA-256 core follows SP800-90A revision 1 section 10.1.2.
- `seedHex` is exactly 64 lowercase hex characters, decoded to 32 bytes and
  supplied as entropy_input; nonce is the empty byte string; personalization
  is exact ASCII `study.uncertainty.hmac-drbg-sha256.v1` (no NUL/newline).
- There is no reseed, prediction resistance or additional input. Every word
  consumes a **separate Generate(8 bytes)** call and its mandatory post-generate
  Update(empty); unused SHA-256 block bytes are discarded. Concatenating words
  from one longer Generate call is a different stream and is prohibited.
- The word is unsigned big-endian u64. Uniform U is `(word >> 11) / 2^53`,
  exactly representable in [0,1). The amount is separately rounded binary64
  `low + (high-low)*U`, without fused multiply-add, clamping or redraw. Rounding
  may make the effective amount equal the declared high endpoint. Facts retain
  the actual canonical effective amount; raw declarations remain in config.
- For row count R, compute threshold=`2^64 mod R` in u128. Reject a word while
  `word < threshold`, then select `word mod R`. Rejection consumes another
  separate Generate(8) call. This is unbiased integer rejection, not a modulo
  shortcut. Samples are generated in increasing index order, then parameters
  in config order; empirical design consumes one accepted row draw per sample.

The user seed is a reproducibility input, **not a claimed entropy source,
cryptographic security guarantee or NIST/CAVP conformance certification**. Core
proof includes NIST's published no-reseed HMAC_DRBG SHA-256 COUNT=0 vector
(entropy+nonce, two 128-byte Generate calls; compare the second). Independent
Python standard-library profile vectors pin 8-byte calls, words, U53 integers,
row selection and the personalization. The algorithm sources are
[NIST SP800-90A revision 1](https://nvlpubs.nist.gov/nistpubs/SpecialPublications/NIST.SP.800-90Ar1.pdf)
and the [official CAVP vector archive](https://csrc.nist.gov/CSRC/media/Projects/Cryptographic-Algorithm-Validation-Program/documents/drbg/drbgtestvectors.zip).
Generator byte equality across platforms and native solve tolerance/replay are
separate qualification claims; one Apple Silicon run establishes neither all
server tuples nor identical floating-point solve bytes on other platforms.

Statistics use sequential Welford mean/M2 in sample order; unbiased sample
standard deviation is `sqrt(M2/(N-1))` and mean MCSE is `SD/sqrt(N)`. Min/max are
sample extrema. Quantile probabilities are fixed 0.025, 0.5 and 0.975 with
Hyndman-Fan type 7: sort observations, use zero-based `h=(N-1)*p`,
`j=floor(h)`, `w=h-j`, then `(1-w)*x[j]+w*x[j+1]`; exact integral h returns x[j].
The helper's N=1 rule returns its sole value; admitted uncertainty requires N>=2.
This matches the [R type-7 definition](https://stat.ethz.ch/R-manual/R-patched/library/stats/html/quantile.html)
without a fuzz correction. Zero sample variance produces SD=MCSE=0. A completed
sample budget is **not convergence**, a confidence interval, distribution/tail
precision or a scientific superiority claim. There is no precision threshold
or adaptive stopping in this version.

Every compile/solve must be finite, valid and residual-checked by the unchanged
compiler/core. Invalid endpoints or draws abort the entire analysis; no redraw,
silent discard, clamp or successful conditional subset is emitted. A sanitized
native `lca.study-analysis-failure.v2` records requested/attempted/valid/invalid
counts, `stopReason:invalid_evaluation` and the offending analysis-local ID;
it contains no source JSON, raw native error, successful facts or business
callback. Successful bundles have exact requested=attempted=valid, invalid=0 and
`stopReason:fixed_budget_complete`.

### Bounds and admission evidence

One monotonic aggregate native budget covers decoding, verification, baseline
recompile/replay, all evaluations, evidence, statistics and serialization, with
pre/post evaluation checks. It never resets per evaluation; the unchanged parent
120-second whole-attempt deadline and kill/reap/RSS/cancellation supervision
remain independently required. Sequential work retains frozen baseline inputs,
one working evaluation model and bounded required evidence. Config/input and
actual serialized evidence are byte/memory checked and output never truncates. Facts retain the exact service-selected batch and
native resource budgets in `executionLimits`, with the functional-unit cap as
a canonical string. Parent-side output validation replays exact seeded amount/row
assignments and binds all method/unit/coverage/parameter/finding/source metadata
to the hash-verified frozen baseline before any future artifact registration.
The 4 MiB artifact ceiling and existing compiler/source/calculation profiles
remain unchanged. Per-attempt `BatchLimits` are explicit service-selected
experiment bounds; they have no permissive runtime defaults. The test experiment
points 8 parameters/17 evaluations and 1024 samples are **not qualified ceilings**.

Read-only schema/component fixtures do not qualify daemon registration, current
source rights, lease-loss/unknown callbacks, real UI/CLI/API/DB/native/report
operation, Linux/Windows capacity or production containment. Those proofs and
service-controlled limits require each owner and actual supported host before
runtime admission. Review and exact workspace integration remain separate.

### Worker-owned report scalar catalog

Facts v2 requires the ordered `factCatalog` of closed `ReportFact` records.
Every report numerical claim selects one exact artifact and `factId`; consumers
copy its canonical value, typed context, unit identity and coverage byte-for-byte.
They do not invent numerical scalar identities, recalculate summaries or derive
report numbers from the raw sample matrix. An ID alone is not a global fact.
All catalog `baselineRunId` values reference the actual frozen baseline Run;
OAT `evaluationId` values remain analysis-local identifiers.

`context.kind` is one of `parameterAmount`, `baselineOutput`, `oatOutput`,
`oatAbsoluteDelta`, `oatRelativeDelta`, `oatSecant`, `uncertaintyStatistic`, or
`uncertaintyQuantile`. Context retains exact parameter/method indexes, endpoint,
evaluation IDs, statistic enum or the fixed quantile probability as appropriate.
IDs are deterministic colon selectors compatible with the existing 128-byte
report ID grammar, for example:

- `parameter:0:low`
- `baseline:method:0`
- `oat:parameter:0:low:relativeDelta:method:0`
- `oat:parameter:0:secant:method:0`
- `uncertainty:mean:method:0`
- `uncertainty:quantile:0.025:method:0`

Catalog order is parameter index then baseline/low/high amount; baseline method
index; then OAT parameter/method index with low output/absolute/relative, high
output/absolute/relative, secant; or uncertainty method index with mean, unbiased
sample SD, mean MCSE, min, max, then quantiles 0.025/0.5/0.975. For P parameters
and M methods, complete catalogs contain `3P+M+7PM` OAT entries or `3P+M+8M`
uncertainty entries. Individual Monte Carlo samples remain immutable typed
artifact evidence and are deliberately absent from this report summary catalog.

Parameter amount units bind the complete frozen resolved exchange Flow,
direction and support-revision chain. Method quantity units bind exact Method
revision and frozen result unit. Relative deltas have `unit:"1"` and
`unitIdentity.kind:dimensionless` while retaining Method provenance separately.
Secants have a typed Method-quantity numerator and exchange-amount denominator;
the exact display unit is `(outputUnit)/(inputUnit)`, not a derivative claim.
Unknown characterized values retain null/unknown and `no_characterized_terms`;
zero-baseline relative deltas retain null, original coverage and
`zero_baseline_relative_undefined`. Partial values remain partial. There is no
ranking fact or hidden omission of undefined entries.

The parent regenerates the complete ordered catalog from the already verified
typed evidence and frozen unit/provenance metadata and requires exact equality.
Missing, duplicate, reordered or extra entries, forged selectors, values, units,
coverage and contexts fail before registration. Conservative serialized catalog
preflight includes actual escaped method/exchange metadata and complete fixed
field overhead; actual final byte/memory and aggregate deadline checks still
apply. Older component receipts precede the catalog and retain their historical
scope. New serialization measurements must bind the updated binary.

### Concrete owner-coordination seams

`study-analysis-record.v2.schema.json` is a concrete proposal for the Edge-owned
V5 analysis record. It contains only analysisId, studyId, the exact independent
profile/config/configHash, the existing Worker execution job/status vocabulary,
and factsRef. FactsRef is nonnull exactly when execution is completed and is
strictly `analysisFacts/lca.study-analysis-facts.v2`; no numerical comparability
or ranking assertion is invented. `PublicAnalysisRecord` is the corresponding
closed DTO. Public negotiation remains Edge-owned and does not register a daemon.

The proposed distinct job is `study.analysis_advanced` in queue `solver`, with
payload schema `study.analysis_advanced.request.v1` and exactly
`{schemaVersion,studyId,analysisId}`. Resource type remains `analysis`.
The proposed service-only functions are:

- `api.svc_study_analysis_advanced_read_input_v1(p_analysis_id uuid, p_job_id uuid, p_lease_token uuid)`.
- `api.svc_study_analysis_advanced_complete_v1(p_analysis_id uuid, p_job_id uuid, p_lease_token uuid, p_completion jsonb, p_idempotency_key text)`.

Argument order is normative for the proposal. Completion is the closed
`study-analysis-completion.v2.schema.json` / `Completion` DTO containing
profileId, configHash, factsRef, storageBucket and storageObjectPath, with the
existing `study:<jobUUID>:<leaseUUID>` idempotency key. Atomic current actor/client/
source, job/lease/resource, cancellation, terminal receipt and unknown-outcome
rules remain required; the proposal is not an installed SQL contract.

Private input v2 retains its minimal six-field analysis record and unchanged
P1 baseline Run/revision/Scenario/preparation projections and exact prepared/
result wrappers. `sourceSnapshot` is now the closed existing P1 projection:
snapshotId, studyId, typed LifecycleModel originRef, sourceArtifactRef,
digestScope, licenseRef, issuerId and observedAt. Exact private `sourceArtifact`
bytes use the original `modelSource/lca.study-source-snapshot.v1` artifact.
Captured origin/license/issuer are checked against those exact bytes, and
observedAt must denote the same RFC3339 instant (the stored timestamptz may
serialize an equivalent offset lexeme).
Authorization remains in the original DB-issued source capability and current
service checks, never a caller-provided success or authorization boolean.
The full private raw snapshot retains its existing protected authorization data;
that raw packet is not a new public contract.

`tests/fixtures/study-analysis/advanced/service-seams.json` supplies concrete
ordered arguments, public completed record, complete private read-input example
and exact synthetic facts registration. `catalog-{oat,uniform,empirical}.json`
and corresponding full facts are producer goldens. They establish component
schema/numerical examples only. Database and Edge must agree and qualify these
seams before actual capability admission. The daemon now implements the agreed distinct advanced seams under the separate default-off admission described in the runtime contract; actual Database/Edge/native qualification remains required.


Cross-owner canonical spelling fixtures preserve 27 boundary vectors and 4096
finite IEEE-754 bit patterns from deterministic xorshift64 (seed
`0x6c63612d73747564`, shifts 13/7/17). Expected strings were generated from the
unchanged owner `canonical_number` function linked to the same locked serde_json
implementation. They validate Node/SQL interoperability, not scientific numbers
or a change to P1 hashing. PostgreSQL shortest notation alone is not equivalent
(e.g. the binary value canonically spelled `1e23`); config hashes always bind
supplied strings unchanged after validation.


The additional exchange-cap synthetic fixture carries 4096 exchanges, 61 inventory
axes, 25 method outputs and 128 views. On the selected macOS arm64 b6 native build,
eight-parameter OAT (17 evaluations) measured 13.35 seconds / 836476 artifact
bytes / 97304576 bytes sampled peak RSS; sixteen-sample uncertainty (17 including
baseline) measured 17.211 seconds / 191918 bytes / 100663296 bytes sampled peak
RSS. Its prepared artifact is 1690339 bytes. These task observations provide a
separate dense multi-method check; the earlier one-method 129-evaluation result
must not be generalized to this shape. Service/operator limits remain explicit
and require the actual target's qualification, including whole-attempt overhead.
