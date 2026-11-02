---
title: Study P0 Calculation Profile and Qualification Baseline
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when qualifying Agent-driven Study numerical semantics before a runtime compiler is introduced
  - when binding the Worker calculation contract from Study consumers
whenToUpdate:
  - when the P0 profile, common cases, evidence scope or legacy-path comparison changes
checkPaths:
  - docs/agents/contracts/study-p0-calculation-profile.md
  - crates/solver-worker/tests/study_p0_benchmarks.rs
  - crates/solver-worker/tests/fixtures/study_p0/**
  - scripts/check_study_p0_benchmarks.py
  - scripts/test_study_p0_benchmarks.py
  - Makefile
lastReviewedAt: 2026-11-02
lastReviewedCommit: d9028e91d549023852dcfb5d37d40faadae3a217
lastReviewedNote: "Worker #353: offline P0 profile and independently derived qualification baseline; no production calculation or admission change."
related:
  - ../../lca-api-contract.md
  - ../../provider-linking.md
  - ../repo-validation.md
---

# Study P0 calculation profile

`lca.study-deterministic-p0.v1` freezes a Worker-owned **offline qualification
baseline** for deterministic Study calculation. Its machine-readable description
is `crates/solver-worker/tests/fixtures/study_p0/profile.json`.
`runtimeEnabled=false`: publishing this contract does not introduce a Study
compiler/API, enable new jobs, change existing solver or admission policies, or
qualify a business pilot. A later owning delivery must provide actual model
compilation, frozen source binding and consumer qualification before enabling a
Study runtime profile.

## Calculation identity and consumer reference

The existing `calculation_contract::descriptor()` and `fingerprint()` remain the
only executable numerical-contract identity. The descriptor schema is
`lca.calculation-contract.v1`, engine `solver-worker`. The fingerprint is
lowercase SHA-256 hexadecimal over the **exact UTF-8 bytes returned by the
existing descriptor function**, with no added newline. It is not a hash of
consumer-reserialized JSON. Consumers must not assume a different object-key
order, implement a competing canonicalization rule, or change the descriptor to
match the reference wrapper.

Edge and Database bind the Worker-issued value through this reference:

```json
{
  "kind": "calculation_contract",
  "schema": "lca.calculation-contract.v1",
  "sha256": "<64 lowercase hexadecimal characters issued by Worker>"
}
```

This example describes the shape; its placeholder is not a usable digest. There
is no invented `artifactId`: the existing descriptor has none. The reference
wrapper does not change the descriptor bytes or fingerprint algorithm. Input,
source, numerical-result and analysis-artifact hashes are separate identities;
none can substitute for the calculation-contract fingerprint. `profileId` is
also separate from the executable calculation identity. Edge owns public
transport and authorization; Database owns immutable persistence and reference
binding; Worker owns descriptor contents, hash generation and numerical rules.
This P0 contract grants no authority to activate a descriptor or execute a job.

## Frozen numerical semantics

- Solve `(I-A)x=y`, then `g=Bx`, `h=Cg`. Preserve instance axes even when multiple
  instances reference one exact Process. A source UUID alone is not an instance.
- Snapshot `B` uses gross source amounts for elementary exchanges: Input versus
  Output does not itself flip the amount sign. Explicit negative source amounts
  remain negative. Signed technosphere incidence and directional LCIA matching
  are separate from this biosphere-amount rule. The negative-inventory fixture
  uses an explicit negative `B` amount, not an inferred Input-direction sign.
- Allocation uses `tidas-reference-allocation-v5`; signed linking uses
  `signed-flow-balance-v1`. Targeted allocations select the reference target per
  exchange; explicit zero and an omitted target in a closed vector both mean
  zero attributed burden. They must not default to one. Existing legacy rules
  remain those of the active resolver, not a new interpretation in a client.
- A finite nonzero reference normalizes by its absolute amount while preserving
  its signed pivot. Input/treatment references stay negative; they are not
  silently reinterpreted as product outputs. Already-materialized allocated
  inventory must not receive allocation a second time.
- Source Process, Flow, property and reference-unit evidence must be explicit
  and version-bound. Missing or conflicting unit evidence cannot be resolved by
  matching display labels or silently choosing the latest version. This is an
  intended Study qualification requirement, not a new production admission rule.
- Ordinary LCIA retains `lcia.nonregional-source-defaults.v1`. Missing factors
  mean `incomplete_coverage_not_zero`; a known factor with numeric zero is a
  different fact. Existing production method-coverage and geographic policies
  remain unchanged. Profile diagnostics do not introduce publication gates.
- Offline numerical comparisons use `abs_delta <= 1e-9 + 1e-9*abs(expected)`
  and an absolute `(I-A)x-y` residual of `1e-12` for these small, well-conditioned
  fixtures. These tolerances do not qualify arbitrary ill-conditioned models.

## Existing paths and migration comparison

The source revisions are recorded in `profile.json`; local checkout state is not
a production deployment claim.

| Concern | CLI auto-build / resulting-process | Platform Model path | Worker snapshot/core path and P0 boundary |
| --- | --- | --- | --- |
| Activity | `lifecyclemodel-auto-build.ts:computeMultiplicationFactors` propagates once through reversed order; remaining cycle nodes receive deterministic order | `matrixCalculation/compile.ts` builds product views; `solve.ts` solves LU | Core solves sparse matrices; no Study model compiler introduced here |
| Cycles | Propagation is not a simultaneous cycle solution; resulting-process consumes supplied multipliers | Fully solves supported cycles and checks activity/result validity | Common equations exercise the existing core, not equivalence of client compilers |
| Allocation | Resulting-process preserves/transforms allocation evidence during scaled aggregation, explicitly `allocation-not-applied-by-builder` | Compiles per-product attribution and independently solves primary/secondary scenarios | Allocation v5 resolves the selected reference; existing materialized Process handoff applies no second allocation |
| Instances | Resulting-process uses model instance IDs; auto-build graph is keyed by source Process IDs | Same-source model instances remain independent | Snapshot numerical axes are Process identities; model-instance-to-materialized-Process mapping remains a future compiler responsibility |
| Reference | Resulting-process chooses the declared instance and preserves a boundary projection; legacy absent-reference fallback remains intentional | Reference amount/direction, model target and product views drive scenarios | Signed reference normalization is tested; it is not a complete reference-product selection algorithm |
| Identity / units | Builder exact-version and cancellation guards have their own projection purpose | Connected ports validate Flow versions | Snapshot release metadata resolves exact support identities; P0 diagnostics explicitly forbid guessed unit equivalence |
| LCIA gaps | Model construction is not LCIA qualification | Existing evidence path reports incomplete LCIA | Version/direction factor matching and known zero stay distinct; missing factors do not become zero scores |

The differences are not all defects. A projection builder, product-system compiler
and full-library snapshot have different input and output contracts. Migration
must compare matching semantic stages, then independently check inventory,
activities, impacts, units, diagnostics and provenance; agreement of one total
is insufficient.

## Common fixtures and independent expectations

`benchmarks.json` has four manually derived sparse systems, four source-shaped
allocation/reference cases and seven identity/coverage diagnostic cases. Exact
numbers are rational strings. Each successful system includes equations and
independently supplied `x/g/h`; the Python checker verifies exact equations and
nonsingularity without importing Worker, CLI, Platform, NumPy or Brightway and
without generating/replacing expectations. It is test tooling, not a new solver.

The Rust integration test consumes the same bytes, calls the existing
`SolverService`, allocation resolver and signed-reference primitives, and
compares their actual output with the independent expectations. Its optional
native-result file binds the exact fixture/profile byte hashes and the existing
Worker descriptor bytes/hash. The Python reader checks both identities, all
required cases, every vector and residual; a partial or stale result fails.
This file is a qualification result from a trusted test invocation, not signed
proof that an arbitrary supplied JSON file came from a particular binary.

| Boundary | Evidence supplied here | Additional production/source test |
| --- | --- | --- |
| Two-node loop, self-reuse | Exact equations plus actual sparse core `x/g/h` | Platform `matrixSolveFixtures` cycle and singular/negative cases |
| Coproduct burdens, Input pivot, explicit/sparse zero | Shared source-shaped rows plus actual allocation and signed-reference primitives | `snapshot_builder` allocation parity, input/output targets, zero-provider-closure cases |
| Repeated source | Two independent core axes and independent demands | Platform repeated-instance and exact-port-version cases; compiler mapping remains unqualified |
| Unit/version mismatch | Offline declared diagnostic expectations, not a runtime request adapter | Existing `snapshot_builder` `release_metadata_*`, `source_closure_exact_reference_*` and Platform port validation tests |
| Missing versus zero factor | Offline version/direction diagnostic cases and signed/known-zero numerical case | Existing `snapshot_builder` `factor_coverage_*` and `nonregional_*` tests |

Allocation primitive normalized rows are not final projected inventories: the
companion product rows remain visible for checking source attribution and must
not be described as a complete assembled product system. Diagnostic strings in
the fixture are test vocabulary, not new public API error codes.

The pre-existing `platform_allocation/materialized.json` contains four allocated
Process cases. Platform `util_calculate.test.ts` can export them using
`ALLOCATION_PARITY_OUTPUT`; Worker can consume that exact file with
`WORKER_PLATFORM_ALLOCATION_FIXTURE`. Its signed-balance checks prove the limited
materialized exchange handoff, not shared source-model compilation.
Brightway25 reconstructs **the same snapshot matrices**, so it can independently
check solve/inventory/impact arithmetic but cannot independently prove the source
model compiler, allocation decision, background choice or methodology.

## Real candidate and business qualification

`historical-cement-candidate.json` retains public historical **metadata and defect
evidence**, not a new qualified dataset. It identifies a 2008 shaft-kiln 42.5MPa
cement Process from Data commit `367642d`, 1000 mass units of output, and its thesis
source. The Process declares free use and no dataset copyright; the repository
has an MIT notice. Those declarations do not establish permission to redistribute
the original paper or every third-party support asset. No paper, private session,
authenticated production payload or credential is embedded here.

The historical XML URI closure has 30 files present but 26 declared-reference
version mismatches, including the mass and energy properties. Product inputs do
not bind explicit background suppliers. File existence therefore does not prove
exact source closure or cradle-to-gate coverage. A current Platform export is a
different source identity and requires its own exact-version and disclosure
checks. No version fallback may be relabeled as the requested historical data.

Business qualification remains `not_qualified` until a named owner approves the
functional unit, scope, source/support/method identity, background binding or
justified exclusions, dataset disclosure and independent inventory/result
expectations, followed by actual compiler/consumer execution. Synthetic tests
and the historical candidate cannot be used as substitutes for that receipt.

## Verification

Use the commands and evidence limits in `docs/agents/repo-validation.md` under
“Study P0 offline qualification”. No database, object store, account, containers
or production requests are needed for the common fixtures.
