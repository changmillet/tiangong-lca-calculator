---
title: Provider Linking Runtime Contract
docType: contract
scope: repo
status: active
authoritative: false
owner: worker
language: en
whenToUse:
  - when explaining or changing calculator provider-link runtime behavior
  - when changing snapshot_builder provider rule defaults or candidate filtering
  - when reviewing provider decision diagnostics, A-write coverage, or provider closure
whenToUpdate:
  - when provider candidate eligibility changes
  - when provider rule defaults or resolution order changes
  - when provider decision diagnostics change
checkPaths:
  - docs/provider-linking.md
  - docs/provider-linking.en.md
  - docs/implicit-regional-supply-mix-modeling.md
  - docs/implicit-regional-supply-mix-modeling.en.md
  - docs/matrix-readiness-report-contract.md
  - crates/solver-worker/src/bin/snapshot_builder.rs
  - crates/solver-worker/src/compiled_graph.rs
  - crates/solver-worker/src/snapshot_artifacts.rs
lastReviewedAt: 2026-11-20
lastReviewedCommit: eb84fc9ba7cfea3f41124ed00647617e36e87941
lastReviewedNote: "Worker #381: checked explicit Model supply, open exchanges, signed eligibility, volume interpretation and historical allocation against the PR implementation."
related:
  - docs/provider-linking.md
  - AGENTS.md
  - docs/implicit-regional-supply-mix-modeling.md
  - docs/implicit-regional-supply-mix-modeling.en.md
  - docs/matrix-readiness-report-contract.md
---

# Provider linking runtime contract

[中文](provider-linking.md) · [Modeling an implicit regional supply mix](implicit-regional-supply-mix-modeling.en.md)

Worker links technosphere exchanges when building a snapshot and records the activity requirements between processes in matrix `A`. The solver uses the frozen matrix. In a primary Model result, internal deliveries specified in the graph have already canceled against consumption, and the inventories of the connected processes already contribute to the result. The following rules apply to exchanges at the result boundary that still need supply or treatment. They also apply to open exchanges in ordinary Processes.

## Determine which exchanges need linking

Within the calculation scope, one complete TIDAS Process revision has one process index and one matrix column. Its `quantitativeReference.referenceToReferenceFlow` must identify a single quantitative reference exchange in that column with a finite, nonzero calculation amount. A reference may be an input or an output. Other exchanges in the column do not create independent matrix columns. To supply a coproduct independently, a complete Process revision must identify it as its quantitative reference.

Worker takes the calculation amount from `resultingAmount`, `meanAmount` or `meanValue`, in that order. The direction sign is `−1` for inputs and `+1` for outputs. Normalization by the absolute raw reference coefficient leaves a reference coefficient of `−1` or `+1`. Other exchanges also receive the allocation fraction attributable to the current reference product.

```text
raw_coefficient = direction_sign * calculation_amount
reference_scale = 1 / abs(raw_reference_coefficient)
c_r = raw_coefficient * reference_scale * selected_allocation_fraction
c_i = sign(raw_reference_coefficient)
```

Flow type determines the next step. Nonzero, non-reference product and waste exchanges form residuals `c_r`. Elementary flows enter the environmental inventory matrix `B`. Other flows are retained for reporting only. An explicit or sparse zero allocation adds no request-root dependencies, matching counts or entries in `A` or `B`. See [Product allocation and historical formats](#product-allocation-and-historical-formats) for field and compatibility rules.

## Find eligible reference exchanges

Ordinary public calculations admit Processes with `state_code = 100`. An owner-draft scope also admits state `0` owned by the scoped user. The reserved publication range `101..199`, including published Result `120`, does not confer numerical eligibility. A Process outside the calculation scope does not appear as a rejected candidate.

Worker indexes quantitative reference exchanges by `(Flow UUID, resolved version)`. An explicit `@version` resolves exactly. An omitted version is resolved once within snapshot visibility and then frozen. Several revisions of the same UUID can coexist when actually used. An eligible reference must match that Flow identity, have compatible units and have a coefficient `c_i` opposite in sign to `c_r`. Listing the Flow only in a non-reference exchange does not qualify a Process to supply it. The labels product, waste, input and output alone do not determine the balancing direction.

To check the sign convention, `Waste Input +1000` and `Waste Output -1000` both give a raw coefficient of `−1000` and can balance a positive residual after normalization. `Waste Output +1000` normalizes to `+1` and can balance a negative residual. An eligible reference in the dependent Process itself can also participate. If selected, its contribution enters the diagonal `A[i,i]`.

## Select providers and determine shares

The default `split_by_process_volume` policy applies the same candidate rules during dependency selection and matrix compilation. Worker records exact-version relationships between resulting and component Processes, including nested relationships, using `referenceToResultingProcess` and `processInstance.referenceToProcess`. The request root identifies the initial Process revision for the calculation. For each open exchange the default keeps eligible results, components and other providers. Each eligible Process revision enters the split once, even when reached along several paths. A shared `model_id` or a provenance relationship gives no preference and causes no exclusion. Earlier default builds preferred providers in the same Model before applying geography.

| Eligible candidates | Default decision |
| --- | --- |
| None | Record `no_provider_candidates` when the Flow revision has no reference exchanges, or `no_opposite_sign_reference` when all have the wrong sign. This balance contributes no entry to `A`. The snapshot boundary policy governs the unresolved exchange. |
| One | The sole provider supplies the whole residual with a share of 1. |
| Several | Select a geographic tier, then normalize annual-volume weights within that tier. |

1. **Supply region.** Use a usable `exchange.location` first, then the location of the Process containing the exchange. If neither is usable, record an unspecified region.
2. **Geographic tier.** Select the first nonempty tier in this order: same subnational area, same country, same region, global, other. Volumes in other tiers do not compete.
3. **Annual volume.** Read the first parseable finite positive number from the provider's `annualSupplyOrProductionVolume` text or multilingual `#text` as its raw weight. The parser reads the leading whitespace-delimited token and removes commas. Unit suffixes do not trigger conversion. Missing, invalid, nonfinite or nonpositive values receive a fixed weight of `1`. If all weights fall back to 1, shares are equal. Partial fallbacks are normalized together with valid values. Diagnostics count the fallbacks. Interpreting the shares requires checking units, periods and reporting boundaries, particularly where separate Process identities may cite the same production figure.

## Write the matrix and check the boundary

Let `w_i` be the normalized supply share. Product allocation has already determined the magnitude of `c_r`. Routing determines which processes meet this requirement. Shares sum to 1 and activity requirements are nonnegative.

```text
activity_requirement_i = (-c_r / c_i) * w_i
A[balancing_process_i, dependent_process] += activity_requirement_i
closure = c_r + sum(c_i * activity_requirement_i) = 0
M = I - A
```

`self_loop_cutoff` is a diagnostic threshold for diagonal risk. The solver determines solvability from the complete `M` with genuine diagonal contributions included.

A saved partly terminated system Process may be used under any applicable downstream snapshot boundary policy. `--technosphere-boundary-policy` determines how Worker handles a nonzero exchange still unresolved in this calculation.

| Policy | Readiness and evidence |
| --- | --- |
| `closed` (ordinary snapshot default) | Requires closure. An unresolved nonzero exchange blocks readiness. |
| `open` | Allows the residual to remain at the system boundary. Retains a warning and per-exchange evidence. |
| `cutoff` | Allows truncation under the declared boundary. Retains a warning and per-exchange evidence. |

An unknown policy is an error. For certificate-grade Scope Closure, the database normalizes omitted and compatible boundary inputs to `cutoff`. Worker only accepts the frozen `cutoff`. Unmatched exchanges, `A`-write coverage and unresolved balances remain reportable. A missing provider alone does not block that certificate.

## Trace an exchange in the diagnostics

Start with the dependent Process and exchange, Flow UUID and version, reference unit, raw direction and normalized residual. Check each candidate's Process revision, reference exchange, coefficient sign, eligibility and rejection reason. The `model_id`, `model_version` and lineage relationships explain candidate provenance. Then inspect the supply region and its source, selected tier, volumes and fallback count, `routing weights`, activity requirements and closure residual. If unresolved, read the specific failure reason alongside the effective boundary policy.

The `decision_kind` distinguishes a unique provider, resolved or unresolved multiple candidates, and no provider. The `resolution_strategy` identifies the method used. Distinguish candidate count from the final matched count. The `residual_edges_total` and `a_balance_edges_written` counters describe residual and written edges. The historical `input_edges_total` and `a_input_edges_written` fields are compatibility projections. Diagnostic exports and readiness reports use the compiled decisions so their explanations trace back to the same evidence. [Matrix readiness report](matrix-readiness-report-contract.md)

## Product allocation and historical formats

The reference exchange itself receives no allocation multiplier. For non-reference exchanges, determine the allocation mode of the Process before selecting the fraction attributable to its reference product.

| Data representation | Handling |
| --- | --- |
| Explicit targets | `allocations.allocation` may be an object or array. Select the target matching `@internalReferenceToCoProduct == referenceToReferenceFlow`. A string or numeric `@allocatedFraction` uses TIDAS `Perc` and is divided by 100. The standard form omits the percent suffix. If nonzero entries sum to 100% and the current target is absent, infer a sparse zero. |
| No allocation declaration | The fraction is 1. The historical scalar `allocations.allocation = {}` is also treated as undeclared and counted in `legacy_empty_allocation_as_undeclared_count`. |
| Historical output shares | Targetless shares declared on separate outputs form one vector totaling 100%. Apply the current reference output's fraction to all non-reference exchanges. A reference exchange without a declared share retains its default of 1. Historical percent suffixes are accepted. Input shares cannot be mixed into this mode. |
| Historical single full allocation | In the absence of a historical output vector or explicit targets, a single full allocation on an input or treatment exchange may be attributed to the uniquely identified quantitative reference. Accept only canonical 100 or the exact string `"100%"`. Count this in `legacy_single_reference_target_inferred_count`. The old `legacy_single_output_target_inferred_count` is a compatibility projection. |

An explicit target must identify a unique input or output exchange whose authored UUID and version resolve exactly to a product or waste flow. Missing versions, substitute revisions, unknown types and elementary-flow targets are rejected under the same checks in ordinary builds and review overlays. Other rejected cases include mixed targeted and targetless declarations, multiple ambiguous targetless entries on one exchange, empty arrays, `[{}]`, incomplete objects, duplicate or unknown targets, nonfinite or out-of-range fractions, and vectors that do not close. Compatibility handling preserves the source JSON.

## Other explicit policies

Under alternative rules Worker first applies `version-exact-lineage-gate-v1`. On a lineage conflict, a resulting Process uniquely selected by the request root supplies the exchange. Otherwise the conflict is recorded as `lineage_overlap_requires_binding`. Candidate counts below refer to candidates remaining after this step.

| Rule | Behavior with multiple candidates |
| --- | --- |
| `strict_unique_provider` | Record an unresolved decision. |
| `best_provider_strict` | Prefer a same-`model_id` subset when available, then score by geography and time. Require a unique winner meeting the minimum score and lead ratio. |
| `split_by_evidence` | Prefer a same-`model_id` subset when available. Split among qualifying providers by evidence score. |
| `split_by_evidence_hybrid` | Prefer candidates with the same `model_id` when scoring. Fall back to equal shares if evidence is insufficient. |
| `split_equal` | Divide equally among eligible candidates. |

## Snapshot identity and data dependencies

The default build records `provider_lineage_policy = volume-lineage-evidence-v2`. The ordinary snapshot source fingerprint includes the build configuration and a digest of the Model content used. The snapshot records its boundary and Flow identity policies alongside the linking policy. Coverage uses `snapshot_coverage.v3`, readiness input and report use v2, and the calculation bundle uses `tiangong.calculation-bundle.v2`. Historical matrices retain their original decisions. Inspect the policy and versions recorded in the individual snapshot when interpreting a result. Admission of an existing snapshot for a new computation is covered by the [calculation identity contract](scope-closure-contract.md#calculation-implementation-identity). For current configuration values, see the [snapshot builder](../crates/solver-worker/src/bin/snapshot_builder.rs).

Worker assigns matrix indices only to Flow revisions actually referenced by the final Process closure. LCIA factors enter characterization matrix `C` only when applicable to the biosphere axis and direction. Active targets must resolve to elementary flows. Required flow property, unit group, source and contact dependencies are resolved with them. Characterization factors do not participate in supplier selection. The builder loads relevant Model revisions in batches and caches nested component relationships for candidate lookups. Time is recorded in `fetch_provider_lineage_sec`. The solver uses the frozen sparse matrix.

Allocation has its own version constraints. New calculations using `tidas-reference-allocation-v5` check the stored allocation marker. Both certificate scope and the frozen data snapshot must declare compatible versions. Deployment requires the corresponding Database and Edge producers of this marker. Historical allocation artifacts remain decodable and readable.
