---
title: Finite Frozen-Input Provider-Impact Diagnostic
docType: contract
scope: repo
status: active
authoritative: true
owner: worker
language: en
whenToUse:
  - when comparing complete candidate Process or Product/Waste Flow bodies against immutable supplied evidence
  - when integrating a local provider-impact report into an authorized caller workflow
whenToUpdate:
  - when frozen input, overlay admission, report binding or offline execution changes
checkPaths:
  - docs/provider-impact-diagnostic-contract.md
  - crates/solver-worker/src/bin/snapshot_builder.rs
  - crates/solver-worker/src/bin/snapshot_builder/**
  - crates/solver-worker/tests/provider_impact_cli.rs
  - crates/solver-worker/tests/fixtures/provider_impact_v1/**
lastReviewedAt: "2026-11-19"
lastReviewedCommit: "3cba273e5e029fadd62278498116cc27240186de"
lastReviewedNote: "Worker #288 review corrected ambient online-argument parsing and froze offline source-closure limits. Native provider/matrix semantics, online configuration, numerical fingerprint and ownership boundaries remain unchanged."
related:
  - AGENTS.md
  - .docpact/config.yaml
  - docs/provider-linking.md
  - docs/matrix-readiness-report-contract.md
  - docs/agents/repo-validation.md
---

# Finite Frozen-Input Provider-Impact Diagnostic

`snapshot_builder` can compare a supplied immutable baseline with full candidate
bodies, compiling every effective Process in that finite universe twice. This
local diagnostic reuses native signed-flow selection, version-exact lineage,
source closure, unit resolution and sparse matrix assembly. It does not fetch an
online census or attest that the supplied effective Process axis matches a live
database selection.

[Worker #288](https://github.com/tiangong-lca/worker/issues/288) defines the diagnostic. Consumers verify the online census, current ownership and authorization before using its findings for a repair.

## Public command

```sh
snapshot_builder \
  --provider-impact-input /absolute/path/request.json \
  --provider-impact-input-sha256 <sha256-of-original-file-bytes> \
  --provider-impact-out /absolute/path/new-report.json
```

All three arguments are required together. Additional explicit snapshot options
are rejected. The frozen request owns the entire policy; ambient database/storage
configuration is not used. A dedicated parser accepts these three arguments before online environment-backed settings are parsed. Frozen source closure uses the compiled 1 GiB document-byte limit independently of `SOURCE_CLOSURE_TOTAL_DOCUMENT_BYTES`; online snapshot builds retain their configured limit. Execution returns before database, queue, snapshot,
cache or object-storage clients are initialized. The command writes one local
report atomically, refuses to overwrite an existing output, and does not write a
report on invalid input. The output directory must already exist.

Only `mode: "matrix_only"` is supported. Both phases assemble matrices and run
native structural readiness checks with `run_factorization=false` and zero unit
samples. They never prepare UMFPACK, solve, enqueue a fallback, or read a cached
online snapshot. A singular matrix can therefore produce a diagnostic report;
its numerical stability remains **not evaluated**. Use a platform-qualified `snapshot_builder` binary with its required native-library closure, including for matrix-only startup.

## Input v1

The schema discriminator is `worker.provider-impact.input.v1`. The exact typed
input is defined in `bin/snapshot_builder/provider_impact.rs`; unrecognized outer
fields fail. [The synthetic singular fixture](../crates/solver-worker/tests/fixtures/provider_impact_v1/singular-template.json)
is a complete example, with only `expected_worker_sha256` left for the actual
executable. Fixture UUIDs and quantities are synthetic test data.

| Field | Meaning |
| --- | --- |
| `schema_version`, `mode` | Exact v1 discriminator and `matrix_only`. |
| `expected_worker_sha256` | SHA-256 of the exact executing file's raw bytes; package version alone is insufficient. |
| `baseline_sha256` | Canonical JSON hash of the complete `baseline` object, including scope, axes, row metadata, documents and policy. |
| `baseline.actor_user_id` | Actor whose state0 drafts may appear; this is a local input assertion, not authentication. |
| `baseline.scope_manifest` | Exact native `lca.data_scope.manifest.v2` with `public_state_100_or_authenticated_owner_state_0.v2`: public state100 OR actor-owned state0. Team/review metadata does not widen or narrow that predicate. |
| `baseline.effective_processes` | Nonempty exact selected axis of `{process_id, process_version}`. The axis must contain every visible Process UUID in the supplied census exactly once, at its latest eligible version. Duplicate exact rows, omitted identities and earlier selected revisions are rejected. |
| `baseline.request_roots` | Fixed exact subset of that axis, passed unchanged to native closure and lineage in both phases. Empty means no request-root restriction. |
| `baseline.omitted_flow_resolutions` | Explicit `{id, version}` answers for omitted Flow versions. Required when encountered; missing/duplicate answers fail. Explicit references always retain their own version. |
| `baseline.documents` | Complete bounded source universe, described below. |
| `baseline.models` | Exact Lifecycle Model documents required by effective Process associations. |
| `baseline.native_policy` | Exact current v1 policy shown in the fixture; alternative rules or overrides reject. |
| `overlays` | Full-body replacements for existing admitted identities, or an empty array for baseline diagnostics. |

Each `documents` member has:

- `identity: {dataset_type, id, version}`; types are `process`, `flow`,
  `flowproperty`, `unitgroup`, `source`, `contact`, `lciamethod`.
- `document` (complete JSON body) and `document_sha256` (canonical hash).
- Frozen row metadata: `state_code`, `user_id`, `model_id`, `model_version`,
  `team_id`, `review_id`; optional values may be null. Metadata is hash-bound and
  cannot be overlaid.

Every document's embedded UUID/version must match its outer identity. Process and
Flow rows use the existing native state100/owner0 visibility checks, including
unselected supplied revisions. Explicit historical source revisions may coexist;
they do not enter the effective Process axis or compiler unless native source
closure requires them. Required dependencies must be in this finite map. Source,
Contact and other support traversal retains the native reference policy's required
and optional roles; an optional reference stays optional. Missing required bodies
or incomplete Flow reference-property/unit chains fail with no live fallback.

Each `models` member has `{id, version, document_sha256, document, user_id,
state_code}`. The model set must cover the effective Process rows' model
associations. Additional visible models contribute to the impacted-model inventory. Required resulting/component Process versions must be supplied.
Model body, UUID/version, native fallback from absent `model_version`, and the
fixed request-root context feed the existing lineage index. Reported overlap is evidence for inspection. The default volume rule can
resolve the exchange using all eligible candidates even when their inventories share a source. Models cannot be candidate overlays.

### Policy and overlays

V1 retains native `split_by_process_volume`, opposite-sign reference-port
eligibility, `volume-lineage-evidence-v2`, strict reference normalization and
allocation, exact Flow revisions/units and native source-reference policy. Boundary
is fixed `closed`, self-loop diagnostic cutoff `0.999999`, singular epsilon
`1e-12`, and no LCIA methods/factors. V1 is not a weighting, provider-binding,
boundary-policy or scientific-method selection API. Native annual-volume fallback
counts are reported as fallback evidence, never as observed annual supply.

Each overlay is `{identity, before_sha256, candidate_sha256, document}`:

- The identity must already be an actor-owned state0 Process or Product/Waste
  Flow in the frozen universe; a Process must also be on the effective axis.
- The baseline hash must match, the candidate hash must match the complete body,
  and UUID/version must remain exact. Duplicate overlays fail.
- Product/Waste Flow type must remain unchanged. Elementary/other Flow, support
  document, LCIA method/factor and Lifecycle Model overlays are unsupported.
- Replacement updates the source map used by *all* compiler reads, including unit
  chains and final source evidence. The compiler never mixes old online bodies
  with candidate evidence. Every overlay must be present with the expected hash
  in both phases' native source evidence, or the command fails.

No caller-provided affected-process list is accepted. Every effective Process is
recompiled, including disconnected consumers. The informational affected set is
the union of changed Process bodies, exact Flow revision users, changed native
consumer decisions/balances, and transitive downstream consumers on either phase's
chosen edges. It conservatively includes downstream users even when their local
routing is unchanged; it does not claim an LCIA impact delta.

### Hashes and limits

Canonical JSON is the Worker's `calculation_evidence::canonical_json_bytes`:
recursively sort object keys, preserve array order, then serialize compact UTF-8
JSON with `serde_json`. Hash those bytes with SHA-256. Numbers follow that
serializer; clients must verify compatible encoding, not assume arbitrary JSON
serializers produce the same bytes. Raw input and executable hashes use raw file
bytes. The example fixture carries independently generated golden baseline/body
hashes verified by the integration test.

Input is at most 64 MiB, with at most 10,000 source documents plus models and at
most 10,000 overlays. Native closure retains its existing frontier/reference
bounds. Output is capped at 128 MiB and never silently truncated. These are local
admission bounds, not a production capacity qualification. Immutable input
acquisition, trusted runtime identity distribution and safe handling of report
contents belong to the caller.

## Report v1

`worker.provider-impact.report.v1` contains:

- Input, baseline and actual executable hashes; the full selection context and
  native policy; all supplied source identity/hash membership; exact model
  evidence and before/candidate overlay hashes.
- `before` and `after`: native effective Process/Flow axes, requested-root closure,
  reference ports, candidate/selected/rejected provider decisions and reasons,
  lineage evidence, weights/fallback counts, unresolved balances, full source and
  inventory-exchange evidence, matrix entries, coverage and structural readiness
  facts/findings/blockers. Matrix triplets are sorted for deterministic reports.
- Exact affected Process UUID/version pairs, visible Model revisions referencing an affected Process, and explicit operation counts. Phase
  numeric counts come from native readiness execution facts; the offline source
  adapter cannot perform database, queue or object-store operations.
- `report_sha256`: canonical hash of the report with this one field removed.

`status: "diagnostic_complete"` means the finite diagnostic ran, including when
readiness findings or blockers exist. It is never a candidate repair qualification
or save/publication gate. Scope always states:

```json
{
  "kind": "supplied_finite_universe",
  "frozen_selection_contract": "worker.provider-impact.frozen-census.v1",
  "online_census_verified": false,
  "online_effective_selection_verified": false,
  "scientific_qualified": false,
  "save_admitted": false,
  "publication_qualified": false,
  "numerical_stability": "not_evaluated",
  "lcia": "not_evaluated"
}
```

Consumers requiring a complete effective Process axis, latest omitted Flow resolutions
and visible Model reference checks require the exact `frozen_selection_contract`
value above. Older experimental reports lack this field.

No generic readiness `publish_ready` or `next_action` conclusion is promoted into
this contract. This command does not run full TIDAS schema validation. Required
online census, authorized acquisition, CLI/Foundry public integration, scientific
rules, numerical qualification and runtime/native retest remain separate evidence
and acceptance conditions. There is no HTTP/queue interface, remote writer,
execution ledger, deployment or release action in this increment.
