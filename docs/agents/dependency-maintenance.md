---
title: Worker Dependency Maintenance
docType: guide
scope: repo
status: active
authoritative: false
owner: worker
language: en
whenToUse:
  - when reviewing dependency proposals or changing dependency validation
whenToUpdate:
  - when monitoring scope, grouping, cadence or synthetic proof changes
checkPaths:
  - .github/dependabot.yml
  - .github/workflows/python-validator.yml
  - docs/agents/dependency-maintenance.md
lastReviewedAt: 2026-10-16
lastReviewedCommit: 3abc6502b0582448a562c4263e63215196eb184a
lastReviewedNote: 'Worker #322: bounded Cargo/pip/Actions monitoring reuses Rust CI and adds scoped synthetic Python validation; native, data, model and hosted qualification remain separate.'
related:
  - ../../AGENTS.md
  - repo-validation.md
  - ../../tools/bw25-validator/README.md
---

# Dependency maintenance

Dependabot monitors the Cargo workspace on Tuesdays at 04:00, the standalone
`tools/bw25-validator` pip manifest at 04:20, and GitHub Actions at 04:40, in
Asia/Shanghai. Each entry uses a seven-day version cooldown. Cargo and pip each
permit two open version proposals; Actions permits one grouped proposal.
Arrow/Parquet, native artifact libraries, async Rust libraries, Python numerical
packages and Python artifact-access packages have related patch/minor groups.
Package majors remain individually visible. Review pre-1.0 minor updates for
compatibility as well. Preserve existing full-SHA Actions references.

The root Cargo lock and manifests remain the Rust resolution inputs. The Python
tool has no committed lock; its ignored `uv.lock` is a local resolution artifact,
and pip monitoring covers its `pyproject.toml`. Do not introduce another package
graph or update dependency/toolchain pins as part of monitoring configuration.
Cargo.lock-only governance already needs no documentation metadata changes;
existing manifest, source and runtime review requirements remain active.

## Existing and focused validation

Rust proposals reuse `ci.yml`: installed SuiteSparse/OpenBLAS/LAPACK identity is
bound into dependency-cache lookup, then formatting, strict Clippy and all-feature
tests run on every main-target PR. A cache hit does not skip these checks.
Monitoring does not change those steps or add a general required check.

`python-validator.yml` checks the actual Python package on Linux x64 with Python
3.12 for changes under the validator, its workflow or monitoring configuration.
This includes human-authored changes. It installs the declared dependencies,
runs `pip check`, and exercises the existing matrix/validation implementation
using a two-process synthetic Brightway solve, independent expected inventory
and LCIA values, incorrect numerical evidence, a dimension guard, compressed
HDF5 round-trip and all three CLI help surfaces. The smoke suite blocks its
in-process HTTP/DB/S3 access and uses a temporary Brightway directory. It requires
no production credentials or datasets. Unrelated feature work runs no new
Python job. Manual dispatch remains available for exact candidate qualification.

To reproduce in an isolated Python 3.12 virtual environment:

```bash
python -m pip install ./tools/bw25-validator
python -m pip check
python -m unittest discover -s tools/bw25-validator/tests -v
```

Linux x64 selects the existing `pypardiso` dependency marker; a macOS ARM64 local
pass does not prove that Linux solver installation. Record the hosted Linux
result before accepting a Python proposal. This small fixture checks library
compatibility, not full-library numerical parity, workload memory, supported
GPU behavior, model qualification or frozen real-data acceptance. Those retain
the existing explicit validation contracts and authorizations.

## External inputs and acceptance

The external Toolkit executable and its asset identity remain separately owned
adoption inputs. Preserve `TIDAS_BIN`, the exact expected Toolkit version and
describe/protocol evidence. Dependabot also does not update or qualify apt/native
libraries, Rust channel policy, model configuration/weights or production data.
Do not weaken native fingerprints, compiler checks, numerical assertions or
data/asset hashes to accept a proposal.

Configuration activates from `main`. Check repository dependency graph/security
settings and the first actual hosted updater run and candidate lockfile; a local
pass or a merged configuration does not prove updater success. Security triage
is separate from the weekly version queue and cooldown.

Automatic merging is disabled for this rollout. Accept proposals through the
workspace controller's existing-PR preview and bound continuation, preserve
their useful proposal context and exact Issue ownership, and never create a
duplicate PR to work around intake. Before association, establish that any
independent merger is disabled or enforces a live Dependabot exclusion including
queued work. Keep proposals unadopted while that policy is unknown. Child merge,
exact root integration and any deployment remain separate steps.

Reference: [Dependabot options](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference).
