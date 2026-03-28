# Provider Linking Resolution Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make provider-present inputs resolve into technosphere `A` with reasonable, explainable logic, and leave behind durable diagnostics that let us track provider parsing and `A` write coverage over time.

**Architecture:** Keep provider-linking in `solver-worker` snapshot-build flow, but stop treating multi-provider resolution as a black box. First enrich the existing provider decision path with structured outcome / reason / strategy metadata and persist compact diagnostics into snapshot coverage. Then add a replay/report path on the same snapshot input so we can compare rules before switching the production default away from `strict_unique_provider`. After the replay evidence is stable, tighten the candidate strategy and change the default rule with focused regression coverage.

**Tech Stack:** Rust (`solver-worker`), `sqlx`, PostgreSQL JSONB coverage metadata, snapshot artifact/report generation, GitHub Issue workflow.

---

### Task 1: Add structured provider decision outcomes

**Files:**
- Modify: `crates/solver-worker/src/compiled_graph.rs`
- Modify: `crates/solver-worker/src/bin/snapshot_builder.rs`
- Test: `crates/solver-worker/src/bin/snapshot_builder.rs`

**Step 1: Write the failing tests**

Add focused tests near the existing provider-rule tests for:
- `strict_unique_provider` returning an unresolved reason instead of opaque `None`
- `best_provider_strict` distinguishing `no_candidate_ge_min_score`, `top1_below_top1_min_score`, and `top1_top2_ratio_too_close`
- `split_by_evidence_hybrid` recording when it used equal fallback

Example test shape:

```rust
#[test]
fn strict_unique_provider_marks_rule_requires_unique_provider() {
    let resolution = resolve_multi_provider(
        ProviderRule::StrictUniqueProvider,
        &exchange,
        &[1, 2],
        &process_meta,
    )
    .expect("resolve");

    assert_eq!(
        resolution,
        MultiProviderDecision::Unresolved(ProviderFailureReason::RuleRequiresUniqueProvider)
    );
}
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p solver-worker strict_unique_provider_marks_rule_requires_unique_provider -- --nocapture
```

Expected:
- FAIL because `resolve_multi_provider` still returns `Option<MultiProviderResolution>`

**Step 3: Write the minimal implementation**

In `crates/solver-worker/src/bin/snapshot_builder.rs`:
- replace `Option<MultiProviderResolution>` with a structured decision enum such as `MultiProviderDecision`
- add typed reason / strategy enums for unresolved and resolved multi-provider outcomes
- thread those fields into `CompiledProviderDecision`

In `crates/solver-worker/src/compiled_graph.rs`:
- extend `CompiledProviderDecision` with:
  - `decision_kind`
  - `resolution_strategy`
  - `failure_reason`

Keep existing allocation output untouched so later tasks do not have to rebuild the matrix-writing path.

**Step 4: Run targeted tests to verify they pass**

Run:

```bash
cargo test -p solver-worker provider_rule_parse_supports_new_modes best_provider_strict_selects_single_top_candidate strict_unique_provider_marks_rule_requires_unique_provider -- --nocapture
```

Expected:
- PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/compiled_graph.rs crates/solver-worker/src/bin/snapshot_builder.rs
git commit -m "feat: add structured provider decision outcomes"
```

### Task 2: Persist explainable diagnostics into snapshot coverage and reports

**Files:**
- Modify: `crates/solver-worker/src/snapshot_artifacts.rs`
- Modify: `crates/solver-worker/src/bin/snapshot_builder.rs`
- Test: `crates/solver-worker/src/snapshot_artifacts.rs`

**Step 1: Write the failing coverage test**

Extend the snapshot artifact round-trip test so coverage includes provider-decision diagnostics:
- `a_write_pct`
- `provider_present_resolved_pct`
- `resolved_strategy_counts`
- `unresolved_reason_counts`

Example payload shape:

```rust
provider_diagnostics: SnapshotProviderDecisionDiagnostics {
    resolved_strategy_counts: BTreeMap::from([
        ("unique_provider".to_owned(), 7),
        ("split_by_evidence".to_owned(), 1),
    ]),
    unresolved_reason_counts: BTreeMap::from([
        ("rule_requires_unique_provider".to_owned(), 4),
    ]),
},
```

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p solver-worker encode_decode_snapshot_artifact_roundtrip -- --nocapture
```

Expected:
- FAIL because `SnapshotMatchingCoverage` does not yet include the new fields

**Step 3: Write the minimal implementation**

In `crates/solver-worker/src/snapshot_artifacts.rs`:
- add a small serializable diagnostics struct under `SnapshotMatchingCoverage`
- use `#[serde(default)]` on all new fields to keep old artifact decoding compatible

In `crates/solver-worker/src/bin/snapshot_builder.rs`:
- aggregate the new `CompiledProviderDecision` fields into compact counts
- compute:
  - `a_write_pct = a_input_edges_written / input_edges_total`
  - `provider_present_resolved_pct = a_input_edges_written / (matched_unique_provider + matched_multi_provider)`
- print the new diagnostics in `write_report_files(...)`

Do not persist every per-edge score into `coverage`; keep `coverage` query-friendly and compact.

**Step 4: Run targeted tests**

Run:

```bash
cargo test -p solver-worker encode_decode_snapshot_artifact_roundtrip -- --nocapture
cargo test -p solver-worker snapshot_builder -- --nocapture
```

Expected:
- PASS

**Step 5: Commit**

```bash
git add crates/solver-worker/src/snapshot_artifacts.rs crates/solver-worker/src/bin/snapshot_builder.rs
git commit -m "feat: persist provider-link diagnostics in snapshot coverage"
```

### Task 3: Add same-snapshot replay and comparison reporting

**Files:**
- Create: `crates/solver-worker/src/bin/provider_link_replay.rs`
- Modify: `crates/solver-worker/src/bin/snapshot_builder.rs`
- Modify: `docs/provider-linking-current-state.md`
- Modify: `docs/sql/2026-03-28-provider-a-metrics.sql`

**Step 1: Write the failing replay test or fixture harness**

Create a focused test or fixture-backed helper that can run the same provider candidate set through:
- `strict_unique_provider`
- `best_provider_strict`
- `split_by_evidence`
- `split_by_evidence_hybrid`

Assert that the report includes:
- resolved edge count
- unresolved reason counts
- equal fallback count

**Step 2: Run test to verify it fails**

Run:

```bash
cargo test -p solver-worker provider_link_replay -- --nocapture
```

Expected:
- FAIL because no replay CLI/helper exists yet

**Step 3: Write the minimal implementation**

Prefer extracting reusable provider-link helper logic out of `snapshot_builder.rs` before adding the new binary.

`provider_link_replay.rs` should:
- load the same process/exchange input shape as snapshot build
- run multiple rules against one selected snapshot scope
- emit a machine-readable JSON summary and a readable markdown summary

Update `docs/sql/2026-03-28-provider-a-metrics.sql` so the SQL view and the replay output use the same metric names where possible.

**Step 4: Run the replay against the known baseline snapshot**

Run:

```bash
cargo run -p solver-worker --bin provider_link_replay -- --snapshot-id 12f9fbca-9a64-48a5-abbb-8333456fe929
```

Expected:
- a report that reproduces the current baseline and shows the gap between `strict_unique_provider` and the candidate replacement rules

**Step 5: Commit**

```bash
git add crates/solver-worker/src/bin/provider_link_replay.rs crates/solver-worker/src/bin/snapshot_builder.rs docs/provider-linking-current-state.md docs/sql/2026-03-28-provider-a-metrics.sql
git commit -m "feat: add provider-link replay reporting"
```

### Task 4: Switch the production-default provider rule with regression coverage

**Files:**
- Modify: `crates/solver-worker/src/bin/snapshot_builder.rs`
- Modify: `docs/provider-linking-current-state.md`
- Modify: `README.md`
- Test: `crates/solver-worker/src/bin/snapshot_builder.rs`

**Step 1: Write the failing regression tests**

Add tests that cover:
- exact tie candidates staying explainable
- same geography / different year splits
- missing year still resolving under hybrid evidence
- fallback-to-equal remaining explicit in diagnostics

Also add one assertion on the CLI default:

```rust
#[test]
fn snapshot_builder_defaults_to_explainable_multi_provider_rule() {
    let cli = Cli::parse_from(["snapshot_builder"]);
    assert_eq!(cli.provider_rule, "split_by_evidence_hybrid");
}
```

**Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p solver-worker snapshot_builder_defaults_to_explainable_multi_provider_rule -- --nocapture
```

Expected:
- FAIL because the CLI still defaults to `strict_unique_provider`

**Step 3: Write the minimal implementation**

In `crates/solver-worker/src/bin/snapshot_builder.rs`:
- change the CLI default to the replay-selected rule
- keep manual override support through `--provider-rule`
- make the markdown/JSON report show both configured rule and actual fallback usage

Update `docs/provider-linking-current-state.md` and `README.md` to explain:
- why the default changed
- how to force an alternate rule during debugging
- which metrics should be monitored after rollout

**Step 4: Run focused validation**

Run:

```bash
cargo test -p solver-worker snapshot_builder -- --nocapture
cargo test -p solver-worker -- --nocapture
```

Expected:
- PASS

If production-like access is available, also re-run the baseline snapshot and compare:
- `matched_multi_resolved`
- `matched_multi_unresolved`
- `matched_multi_fallback_equal`
- `a_input_edges_written`
- `a_write_pct`

**Step 5: Commit**

```bash
git add crates/solver-worker/src/bin/snapshot_builder.rs docs/provider-linking-current-state.md README.md
git commit -m "feat: switch provider linking to explainable multi-provider default"
```
