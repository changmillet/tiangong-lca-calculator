# Optimization Review (2026-03-05)

This document captures current optimization opportunities for the LCA solver stack, based on
recent p500 runs and Brightway25 cross-validation reports.

## 1. Baseline and interpretation

Reference artifacts:

- `reports/full-run-p500-split/run-20260305T033115Z.json`
- `reports/bw25-validation-p500-split/d1398105-dec7-4a98-bd60-dd3e5623ede9.json`

Key timing observation:

- Rust comparable compute (`solve_mx + bx + cg`) is extremely small.
- End-to-end Rust job time is dominated by non-compute overhead (queue lifecycle + artifact persistence).
- Brightway validation runtime is dominated by artifact load and matrix rebuild, not solver core.

Conclusion:

- Solver math backend is not the main bottleneck right now.
- Highest ROI is in orchestration and I/O path optimization.

## 2. Priority optimization list

### P0 (highest ROI)

1. Split `solve_one` persistence timing in worker diagnostics (Completed 2026-03-05)
- Add separate timings for:
  - HDF5 encode
  - object upload
  - DB insert/update
- Goal: make I/O bottleneck visible per run.
- Implemented in:
  - `crates/solver-worker/src/db.rs` (`lca_results.diagnostics.persistence_timing_sec`)
  - `scripts/run_full_compute_debug.sh` (result timing fields in JSON/MD report)
  - `tools/bw25-validator/src/bw25_validator/cli.py` (`speed_comparison.rust_persistence`)

2. Add benchmark mode for solve jobs (Completed 2026-03-05)
- Skip result upload (or force inline payload) when running benchmarking.
- Goal: compare solver-only and compute-only paths without storage noise.
- Implemented in:
  - `crates/solver-worker/src/config.rs` (`RESULT_PERSIST_MODE=normal|inline-only`)
  - `crates/solver-worker/src/db.rs` (`inline-only` skips artifact encode/upload)
  - `scripts/run_full_compute_debug.sh --result-persist-mode <mode>`

3. Improve full-run report precision
- Current shell report rounds to integer seconds for some fields.
- Use DB timestamps and/or millisecond precision for `prepare/solve`.
- Goal: accurate short-job comparisons.

### P1

4. Avoid unnecessary payload serialization
- If `return_x/g/h` options disable fields, avoid building and serializing unused vectors.
- Goal: reduce CPU and payload bytes.

5. Add queue and DB latency telemetry
- Track time from enqueue to worker pickup, and DB write latency.
- Goal: separate queue pressure from compute issues.

6. Add artifact compression option
- Evaluate `gzip/zstd` for HDF5 payloads depending on data shape.
- Goal: reduce upload time and storage cost.

### P2

7. Batch write path for result metadata
- For high-throughput batch solving, aggregate inserts with lower transaction overhead.

8. Optional local disk staging / L2
- Cache frequent artifacts or factorization metadata on disk for warm restarts.

## 3. Correct comparison rules

Use these lanes explicitly in reports:

1. Comparable compute lane
- Rust: `solve_mx + bx + cg`
- Brightway: `solve_sec` (or `build_plus_solve_sec` when matrix rebuild is part of the chosen lane)

2. Job run lane
- Rust: job `run_sec` (`lca_jobs.started_at -> finished_at`)
- Includes persistence and worker overhead.

3. End-to-end lane
- Request enqueue to final durable result persisted.

Do not mix lanes when declaring “who is faster”.

## 4. Suggested next implementation step

Implement worker-side persistence sub-timings in `lca_results.diagnostics`, then extend:

- `scripts/run_full_compute_debug.sh` to show these fields
- `tools/bw25-validator` to include lane-based comparisons in summary

This gives stable optimization feedback loops without changing solver architecture.
