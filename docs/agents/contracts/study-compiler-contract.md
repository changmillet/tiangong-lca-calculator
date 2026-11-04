---
title: Study Explicit Model Compiler Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when preparing or executing the bounded Study explicit-connection profile
  - when implementing the owned W06 source, artifact or job adapter
whenToUpdate:
  - when Study source, normalized model, input or result schemas change
  - when compiler identities, numerical rules, bounds or qualification evidence change
checkPaths:
  - crates/solver-core/src/service.rs
  - crates/solver-worker/src/study/**
  - crates/solver-worker/src/bin/study_compile.rs
  - docs/agents/contracts/study-compiler-contract.md
lastReviewedAt: 2026-11-05
lastReviewedCommit: 818b5e00a37f56637f6418ead692ab430c1fdbc1
lastReviewedNote: "Worker #366: reviewed export of the original synthetic prepared/result/source bytes already hash-bound by the service fixture. Generator arithmetic, schema, runtime and earlier fixture bytes remain unchanged; these files grant no source or job authority. Worker #368: reviewed Linux missing-VmRSS exit reconciliation bounded by one sample period and the original deadline, cancellation polling, fail-closed other probe errors and kill/reap qualification. Numerical, public schema, source authority and operational admission boundaries remain unchanged. Combined review adopts exact Main f74dfdba1d0f8aa7b87930ca344b2213e21b5f05 into method source 818b5e00a37f56637f6418ead692ab430c1fdbc1; method structural N/W limits and separate actor/cohort/professional qualification remain in force."
related:
  - study-p0-calculation-profile.md
  - ../repo-validation.md
  - ../../lca-api-contract.md
---

# Study explicit compiler

`study::prepare` and `study::execute` implement the bounded local numerical
producer for `lca.study-explicit-v1`, compiler `study.compiler.v1`. They do not
register jobs, activate an API, attest source authorization, or declare a
business/scientific qualification. P0 remains offline and unchanged. W06 must
qualify the runtime adapter before enabling a hosted profile.

## Formats and trusted source boundary

`crates/solver-worker/src/study/contracts.rs` owns strict camelCase DTOs:
`lca.study-source.v1`, `lca.study-normalized-model.v1`,
`lca.study-prepared-input.v1`, and `lca.study-result.v1`. Unknown DTO fields fail;
raw TIDAS source documents preserve their broader source schema.

A SourceInput contains the exact origin LifecycleModel reference, raw UTF-8 JSON
strings and byte hashes of LifecycleModel/Process/Flow/FlowProperty/UnitGroup
revisions, ordered methodRefs, declared cutoffs, amount overrides and optional
frozen StudyBinding. Embedded source UUID/version must match the descriptor.
The compiler parses the actual TIDAS processInstance graph and exchanges; it does
not accept preassembled A/B/C as a source model. Connection Flow versions must
be explicit. When a Flow selects multiple same-direction exchanges, the model
must specify `@exchangeId`; no ambiguous first-match fallback is used.

Source documents also carry server-resolved `stateCode` and `ownerActorId`, with
an optional SourceInput `actorId`. The compiler rejects reserved/public Result
states including 120, and accepts only public 100 or draft 0 whose owner equals
the stated actor. **These asserted JSON fields are not authorization receipts.**
Only W06's independently authorized resolver may supply them at runtime: it
must read actual DB rows, enforce actor/source scope, and immutably bind exact
state/owner observations and origin/artifact identity under the acquisition/job
authority. Caller-controlled compiler JSON cannot bypass that producer. Public
prepared artifacts retain state codes but omit actor/owner IDs and raw documents.

The existing async TIDAS export adapter and original source export artifact
identity require separate qualification. SourceInput's `sourceHash` is SHA-256
of its exact bundle bytes; it is not automatically the original exporter ZIP's
byte hash. W06 must bind original source snapshot -> verified dependency bundle
-> prepared output through owned-task readback/completion. A newly authored
single-instance test LifecycleModel does not prove that an existing dataset
LifecycleModel was resolved. Actual production authoring needs its own authorized
versioned issuer; an arbitrary test origin UUID is never such an issuer.

## Numerical rules

Each activity axis is `(instanceId, pivotExchangeId)`, including same-source
instances. Each instance retains its declared reference view; every explicitly
supplied product adds its own view. These are allocated product-system activity
views, not ordinary snapshot Process columns. Matrix rules build `(I-A)x=y`,
`g=Bx`, `h=Cg` without cutting cycles or propagating one-pass multipliers. Study opts into the
additive core `with_exact_zero_policy` constructor, which prunes only exact zero
and enforces the retained-cache capacity. Existing core constructors keep their
historical 1e-15 threshold. Tiny finite coefficients can have material amplified
impacts and must not silently disappear. This opt-in policy is bound by the Study
compiler/profile identity; no legacy fingerprint or caller changes.

The existing allocation resolver validates declarations and fractions. A view's
pivot never receives allocation. Explicit/sparse zero remain zero. Undeclared
burden belongs to the original reference view; a non-reference targeted product
view receives zero undeclared burden. Independent non-reference products without
valid allocation are rejected. Source inventory is never rewritten or
materialized twice. Companion product views are independent allocated systems;
this profile does not assert physical co-production or expand implicit suppliers.

A signed reference normalizes by absolute amount, retaining an Input/treatment
negative pivot. Connected supply and consumption must have opposite signed
coefficients and exact Flow/unit identity. Multiple providers per consumer port
are unsupported. Every nonzero unconnected non-reference technosphere exchange
requires an explicit located cutoff or declared supply boundary. Cutoff reasons
remain frozen evidence; no provider inference is performed.

Allocation/normalization evaluates amount times share divided by the absolute
pivot using binary mantissa/exponent decomposition. Intermediate underflow or
overflow must not erase a representable normalized value. A final nonzero value
below the smallest f64 subnormal, or above finite f64, is rejected explicitly.
Opposite-sign connection checks compare nonzero signs directly, never a product
whose magnitude could underflow or overflow.

Unit handling supports only exact reference FlowProperty/UnitGroup chains whose
reference factors are exactly one. Incompatible versions, missing support,
non-reference conversions or label-based conversions fail. Elementary B uses
gross source amounts, preserving explicit negatives; direction is retained on
the inventory axis for factor selection. Only the existing verified static bundle
and `lcia_nonregional::prepare_static_method` may issue production factors.
Missing exact method/Flow/version/direction coverage stays in the explicit gap
list; an actual numeric-zero factor is matched coverage.

Overrides are unique finite replacements at
`/instances/<ASCII-id>/exchanges/<ASCII-id>/amount`. Source quantitative-reference
amount overrides, provider changes, parameters, arbitrary JSON paths and other
conversions are rejected. Overrides are sorted and retained even when a change
has the same numeric value as its source. Changing a Scenario requires a fresh
preparation. Functional unit amount is finite, positive and at most 1e12, with
exact reference Flow/unit; ordered methods equal the frozen definition's refs.
Base preparation has no StudyBinding/inputHash and uses unit reference demand.

## Identities and replay

W08 [analysis](study-analysis-contract.md) reuses frozen P1 prepared/result
bytes and execution validation. Its facts-only numeric strings and new config
domain do not change any encoding, semantic hash or calculation rule below.
No live model/factor reload is performed by the analysis path.

Legacy `calculation_contract::descriptor/fingerprint` and global cache/snapshot
semantics are unchanged. Study identities explicitly bind compiler/profile plus
the existing opaque core calculation reference. An extension of the global
calculation descriptor requires its own deliberate compatibility/rebuild review.

Semantic encoding uses UTF-8 compact JSON with lexicographically sorted object
keys, preserved schema-defined array order, literal Unicode, no final newline,
and pinned serde_json shortest-roundtrip finite f64 encoding. Effective amounts
and matrix signed zero normalize to positive zero. Document arrays, instances,
exchanges, ports, cutoffs and overrides sort by their identity/path; method order
is semantic. Raw source document byte hashes remain exact and therefore change
if the original document bytes change, even for a metadata-only change.

The SHA-256 input is `<domain UTF-8> + NUL + <encoded JSON>`. `study.model.v1`
binds model, matrix/axis/demand, coverage, method-source hash, compiler/profile
and calculation reference. `study.input.v1` additionally binds frozen
StudyBinding and modelHash. SourceHash and artifact hashes are exact-byte domains,
without this prefix; none can substitute for model/input/calculation identity.
The independent fixed golden encoding test guards domain separation.

Prepared input persists the full frozen normalized graph and numerical input.
`decode_prepared` verifies exact artifact bytes, schema, current calculation
reference and semantic identities. Replay never queries latest source data.
Identity validation alone does not prove issuance: W06 must bind the typed
artifact to the current owned preparation and its fenced completion.

## Bounds and lifecycle

Default hard maxima: 4 MiB source, 512 documents, 64 instances, 128 views,
4096 exchanges, 25 methods, 32768 matrix nonzeros, 131072 dense cells, 4 MiB
prepared artifact, 256 MiB estimated owned compiler/matrix allocation and 30s
phase budget. Smaller limits are supported; requests cannot raise the ceiling.
Allocation admission estimates source bytes times eight, expanded per-instance
Process JSON bytes times 32 (including allocation metadata and normalization/hash
staging), plus dense cells times 64. A streaming counting writer preflights the
aggregate expanded Process bytes against the artifact ceiling and owned-memory
budget **before any per-instance inventory/allocation cloning**. The matrix
admission includes that retained normalized estimate rather than checking the
stages as separate independent budgets. These conservative estimates are in
addition to source/count/cell/NNZ caps. This estimate is not an RSS claim
for native libraries or the separately verified static-bundle loader. W06 must
apply the process/total RSS, download, upload, concurrency and temp-space bounds.
The initial API/DB artifact transfer ceiling is the same 4 MiB. A larger original
export or compiled artifact is rejected or remains unavailable until a separately
qualified bounded transfer protocol exists; it is never truncated.

Native execution checks finite x/g/h and strictly nonnegative activity (tiny
negative values are rejected rather than clamped because high inventory/factor
intensities can amplify them),
and `(I-A)x-y` max residual `<=1e-12*scale`, `scale=max(1,|x|,|y|)`. Singular,
negative, oversized, nonfinite and excessive-residual outcomes fail; no solver
fallback is used. Time checks occur at phase completion. W06 must run synchronous
native work in a killable subprocess; a Tokio timeout cannot terminate UMFPACK.
Periodic lease renewal, cancellation, retries, storage upload and atomic
service-only completion are W06 responsibilities, not pure compiler test proof.

## Qualification evidence

The source-model tests exercise core solves for cycles/self-reuse, allocation,
independent nonzero repeated-instance demands, signed treatment references,
zero/negative amounts, exact identities, unit conflicts, unsupported overrides,
coverage, artifact integrity and hash stability. The 64-instance/128-view
native envelope test produced a roughly 167 KiB prepared input and solved in
19ms on the task's Apple Silicon local environment, max residual about 5.3e-23.
This is a bounded local observation, not cross-platform capacity qualification.

The task's private-source cement execution uses the exact public Process
01.01.000, nine Flows and four numeric support documents, plus a **task-authored**
single-instance LifecycleModel and explicit four upstream cutoffs. The verified
pinned method/default loader supplies factors. Actual source->compiler->core
execution at 1000kg yields h=[1092.8,1.796202], four inventory axes, five coverage
gaps and zero balance residual. P0 supplies independent expected subtotals only.
The model explicitly carries TASK_AUTHORED_TEST_MODEL alongside the source scale,
workflow, particle, units and coverage findings. It proves bounded compiler
execution from real inventory, not a real hosted source acquisition, authorized
published LifecycleModel, full LCA, scientific review or business acceptance.
Raw acquisition documents and account/identity wrappers remain outside Git. Full offline CLI execution including the verified static-loader phase measured
28.58s wall / 21.74s user and 111820800 bytes maximum resident set size on
Rust/Cargo 1.99.1 arm64; native execution itself was 2ms. The 30s compiler/native
phase checks exclude static preload. W06 must bound and heartbeat preload as well
as enforce total job/process time and RSS, rather than infer a whole-job timeout
from that phase setting.


## Calculation reference extension boundary

This delivery binds the existing `{kind:"calculation_contract",schema:
"lca.calculation-contract.v1",sha256}` unchanged, plus profile/compiler identity
inside both Study hashes. If a separately transported numerical descriptor is
needed, the proposed additive descriptor is `lca.study-calculation-contract.v1`
with exact fields `profileId`, `compilerVersion`, `matrixZeroPolicy:"exact-zero-v1"`,
`allocationSemantics:"tidas-reference-allocation-v5"`,
`linkingSemantics:"signed-flow-balance-v1"`,
`lciaSelection:"lcia.nonregional-source-defaults.v1"`, `coreCalculationContractRef`.
Its distinct reference would have `kind:"study_calculation_contract"`, that
schema and a Worker-issued SHA-256 of the exact versioned descriptor bytes.
This is a proposed transport extension, not an issued additional reference or
permission for consumers to synthesize one. Admitting it requires coordinated
API/Database version qualification; the current typed core reference cannot be
relabeled, globally replaced or silently recanonicalized.


The owned runtime is specified separately in [study-runtime-contract.md](study-runtime-contract.md). Study artifact/source/IPC decoding uses the bounded exact-f64 codec in `study/json.rs`; ordinary serde_json float parsing is not changed. Decimal normalized matrices and nested allocation numbers must retain original f64 bits across serialization, decoding and semantic rehash, as well as pass integer-only fixtures.
