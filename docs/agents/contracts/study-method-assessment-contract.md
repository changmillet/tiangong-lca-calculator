---
title: Study source-bound method assessment contract
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when assessing an issued frozen Study against the independently pinned PEF/EF31 source module
whenToUpdate:
  - when supported assessment predicates, declarations, schemas, source adoption or arithmetic changes
checkPaths:
  - crates/solver-worker/src/study/method_assessment*
  - crates/solver-worker/src/study/method_assessment/**
  - crates/solver-worker/src/study/method_assets/**
  - crates/solver-worker/src/bin/study_method_assess.rs
  - crates/solver-worker/tests/fixtures/study-method-assessment/**
  - scripts/*study_method*.py
  - docs/agents/contracts/study-method-*.schema.json
lastReviewedAt: 2026-11-04
lastReviewedCommit: 3d3159d9f231143a51f761f895fbd3aef18d1b9b
lastReviewedNote: "Worker #366: initial independent assessment contract; software source/byte consistency, professional judgment, actual study evidence, hosted admission and consumer adoption remain separate."
related:
  - study-analysis-contract.md
  - study-compiler-contract.md
  - study-runtime-contract.md
  - ../repo-validation.md
---

# Initial bounded assessment

Worker #366 owns an independent `lca.study-method-assessment-*.v1` family and
`lca.study-method-pef-2021-ef31-cement-intermediate.v1` executable profile. PCR
owns the source meaning. This profile produces source-derived mechanical facts
and explicit pending/unsupported requirements for a declared finished-cement
intermediate product. It never issues PEF conformity, scientific, comparative,
EPD, Commission category-rule or business approval. Existing Run/compiler,
calculation fingerprints, factor selection and hash domains remain unchanged.
The initial library/offline candidate is not the complete #366/#2181 delivery.

## Independently adopted source

The consumer pins PCR Main `51c0842c67b14ba886e2edee2846e26eefac030f`, module
`method.pef-2021-ef31@0.1.0`, delivered artifact
`b19ba5754b0bc20c44c8c65966bafafd5984cd74636f2c805c0fa4373f5554ad`, review basis
`989391423c7f31bdf77e18ea4438fab1a27ff6708c0f44ecad8ab8813763a165` and exact
source-bindings bytes. Caller-supplied package review/scientific flags, filename
or file presence are not trust anchors. The module remains a candidate source
interpretation without scientific study approval.

The producer package payload hash uses its own ordered two-space JSON plus one
newline. It is distinct from Study's sorted compact semantic JSON and from the
whole delivered artifact hash. Embedded authored UTF-8 bytes, including BOMs,
retain their own raw hashes. Runtime imports neither a sibling repository nor
restricted XML/PDF originals. A small reviewed source bridge keeps the official
PM XML hash and the existing data-release XML hash distinct; whitespace-equivalent
structure never rewrites an original pin or proves rights/geographic suitability.

All sixteen primary category and nine decomposition identities are source-bound,
including exact versions/reference quantities/units and the organic-cancer
filename alias. Source NF/weight decimal strings retain their original spelling.
The official NF config's ecotoxicity typo remains source evidence; effective unit
identity follows the separately bound XML/JRC quantity, not an invented conversion.

## Closed request and source proof

Config.v1 binds one canonical baseline Run, exact module reference, explicit
structured declarations and requirement/evidence references. Declarations select
the proposed product/phase/category-rule scope; software does not infer it from
freeform goal/boundary prose. Providing an evidence reference records an unverified
reference only. It cannot establish an EF dataset class, waiver, professional
judgment, representativeness or approval. No category totals or arbitrary factors
are accepted in config. `configHash` is SHA256 of UTF-8
`study.method-assessment-config.v1`, one NUL, then sorted compact semantic JSON
without a newline. Evidence-reference array order is preserved. The independent
`config-identity.json` fixture pins canonical bytes/hash for consumer adoption.

Input.v1 contains an assessment/job/config record, the unchanged frozen P1
Run/revision/Scenario/preparation tuple and exact prepared/result/source private
capabilities. `analysis::validate_input` and `analysis::verify_run` remain the
shared frozen-pair checks through an internal verifier adapter; no W08 analysis
or new Run is registered. The original SourceSnapshot bytes/hash, issuer/license,
origin, observation time and exact document membership/hashes must match. Effective
cutoffs/overrides/methods/StudyBinding come from frozen evidence. Re-preparing
those exact documents with the actual verified pinned static bundle must recover
the same source/model/input/C/method-source identities; there is no latest data
or factor reload and no solver rerun.

The runtime loads the existing pinned static bundle through its hash-verifying
loader. Request-supplied `VerifiedStaticLciaBundle` metadata cannot instantiate
the assessment's sealed trusted source object. Actual selected C entries and
coverage are checked against the existing nonregional policy. Its policy/hash is
not changed. Ambiguous-default provenance retains source location, ordinal,
record hash and alternative count when the existing evidence has them. Unknown
unique-record metadata stays explicitly unavailable. Cache equivalence, blank
locations and matching values never prove geographic/compartment suitability.

## Requirements and professional evidence

Facts.v1 includes every PEF-01 through PEF-20 requirement, original source locators,
explicit applicability/declaration state, separately scoped machine checks and
professional status. A machine check verifies only its named predicate. Presence
of declaration/evidence is `recorded_unverified`, not a satisfied scientific
predicate. Professional evidence remains pending or recorded-unverified; this
initial profile cannot manufacture a verified judgment.

| Requirement | Supported machine evidence and honest limit |
|---|---|
| 01 goal/category rules | Exact frozen goal/definition binding and declared branch; fitness and actual Commission PEFCR applicability unverified. |
| 02 function/unit | Exact frozen functional unit/reference-flow and unit-support binding; supported declared mass/output-product scope; strength, function and cement identity need judgment. |
| 03 stages/boundary | Frozen process graph/connections/cutoffs and source identities; full life-cycle stage applicability/completeness unverified. |
| 04 inventory | Actual frozen processes/exchanges/inventory axes, coverage and gaps; an empty list/zero is never proof of completeness. |
| 05 exclusions | Screening cutoffs violate the source screening rule. Final-study cutoffs require unimplemented cumulative material/energy/all-category significance and hierarchy evidence; mark unsupported/unproven, not a claim that PEF prohibits every cutoff. Background embedded exclusions remain unknown. |
| 06 primary/DNM/DQR | Source metadata/identities only; no self-assigned scores or document-presence pass. Situation/option suitability and whole-study contribution-weighted DQR require evidence not present in ordinary P1 artifacts. |
| 07 no-PEFCR interim | Conditional declared branch; actual searches/availability/non-EF use/disclaimer and DQR/deliverable exceptions unverified. |
| 08 PEFCR interim | Conditional branch; exact applicable category-rule/replacement/verifier evidence unsupported by this common profile. No imported no-PEFCR exclusion permission. |
| 09 dates | Distinct pinned recommendation/corrigendum/interim/factor identities and conditional validity text; no fixed 2028 expiry or automatic currentness claim. |
| 10 allocation | Actual frozen allocation structure/arithmetic evidence; hierarchy/causality/sector choice unverified. |
| 11 modelling | Frozen source/location/time observations when explicitly present; agriculture/electricity/transport/capital/storage/scenario applicability and suitability unverified. |
| 12 circularity | Frozen source observations only; CFF/lifetime applicability, parameters and substitution judgment unsupported/unverified. |
| 13 climate | Selected exact climate parent/decomposition identities and source factors; no fabricated removals/offsets or inferred carbon-balance validity. |
| 14 categories | Exact sixteen primary identities/versions and selected decompositions; no parent/subcategory double counting. Partial selected methods cannot pass full selection. |
| 15 characterization | Exact source/C/flow/version/direction/reference-unit consistency and retained selection metadata; absent CF remains unknown; geography/compartment/unit representativeness remains unverified where source evidence is unavailable. |
| 16 normalization | Source-bound sixteen-category numerical eligibility and NF division only; no sum of unweighted ratios; incomplete categories unavailable. |
| 17 weighting | Source fraction weights once, ordered primary-only aggregation and explicit point-scale identity; no public-comparability judgment. |
| 18 robustness | No arbitrary top-N surrogate; professional robustness/completeness/sensitivity judgment pending. |
| 19 reporting/claims | Frozen numerical evidence is not a report, human verification or certification; actual report/verification/communication requirements pending. |
| 20 rights/owners | Exact independent source and module pins and rights notices; no blanket rights or study approval from hashes, CI or candidate review. |

## Material P1 coverage limit

P1 treats every unmatched Method × inventory key as a gap. It intentionally does
not convert absent CF into justified non-applicability. Input-oriented resource
methods and output-oriented impact methods make complete sixteen-category values
unreachable for ordinary sparse nonempty inventories under this conservative
coverage model without additional reviewed applicability evidence. This is a
product limitation, not merely a test limitation. This assessment does not change
old Run coverage or invent zero factors. A future reviewed consumer profile must
bind any justified non-applicability separately before broader numerical adoption.

The real cement pilot retains two selected methods, four upstream cutoffs and its
actual coverage gaps. It remains negative/incomplete. The user has no complete
real positive case available; this does not waive real positive/professional
acceptance. Isolated arithmetic/software fixtures cannot substitute for that case.

## Candidate normalization and weighting

Only exact source-compatible complete values for all sixteen primary categories
are eligible. Missing methods, wrong versions/units/source, gap/unknown values or
unsupported selection prevent the candidate. Nine subcategories remain explicit
decompositions and never enter N/W or totals.

Compute `normalised_i = characterised_i / NF_i`, then
`weighted_i = normalised_i * source_weight_fraction_i`, and sum only these sixteen
weighted values in source N/W order. There is no second division by100 or sum of
unweighted normalized values. Characterised values retain method units;
normalized ratios have unit1 with the source global per-capita basis. Weighted
categories and the candidate single score use an explicitly defined unscaled Pt
identity: one Pt is the weighted sum using these fraction weights; no implicit
mPt conversion. Intermediate nonfinite or nonzero-underflow arithmetic fails.
Canonical output strings use the unchanged numerical facts codec. Source NF/weight
strings, ordered mapping and scaling/basis remain in provenance.

Candidate arithmetic is not a conforming PEF score or approval. The isolated
Decimal oracle tests numerical mappings/scaling; assessment-pipeline positives
claim only actually verified predicates. Scientific/ranking/business approval
flags remain false, including when every software predicate passes.

## Offline invocation and wire contracts

The four checked-in schemas close the new config, facts, private-input and
offline-candidate envelopes. Retained P1 nested rows keep their owner shape;
byte/source/config verification remains necessary after schema validation.
`study_method_assess --input <candidate.json> --prepared <original-prepared.json>
--result <original-result.json> --source-snapshot <original-source.json>
--static-cache <verified-cache-root> --output <new-facts.json>` consumes local
files only. The cache root contains `lciamethods/cache_manifest.json`. It never
issues a job, lease, source capability, actor grant or registered assessment.
Output creation is exclusive, bounded and private on Unix; existing paths are
never overwritten. An incomplete write remains for explicit operator handling.
Its receipt is an offline candidate receipt, not a service completion receipt.

## Later owned runtime and consumer work

Proposed job `study.method_assessment`, payload
`study.method_assessment.request.v1`, and distinct `methodAssessment` resource
require explicit Database/Edge agreement. Proposed private read/completion seams
are `svc_study_method_assessment_read_input_v1` and
`svc_study_method_assessment_complete_v1`. Do not register/advertise them from this
initial milestone. Current source/actor/client/lease, cancellation, unknown-outcome
retention, artifact bytes and report disclosure must be qualified through their
owners before hosted admission. Do not widen V5 or use an offline CLI as service,
API, Database, report or complete #366/#2181 acceptance.
