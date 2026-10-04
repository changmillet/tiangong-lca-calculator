#!/usr/bin/env python3
"""Qualify the exact Worker summary SQL in an explicitly owned disposable Docker PG.

No remote URLs are accepted. The caller owns container creation and receipt-based
release. Creates only a new synthetic database inside the verified container.
"""
import argparse
import collections
import json
import pathlib
import re
import subprocess
import uuid

ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE = "crates/solver-worker/src/package_retention.rs"
AS_OF = "2026-10-04 00:00:00+00"
OLD = "2026-08-01"
RECENT = "2026-09-04"  # Exactly 30 days before as_of: protected.


def run(command, *, sql=None, check=True):
    return subprocess.run(command, input=sql, text=True, capture_output=True, check=check)


def query(source):
    body = source.split("pub async fn fetch_package_retention_summary(", 1)[1]
    sql = body.split('r"', 1)[1].split('\n        ",', 1)[0]
    for key, value in [("$1", f"'{AS_OF}'::timestamptz"), ("$2", "30"), ("$3", "30")]:
        sql = sql.replace(key, value)
    assert sql.strip().startswith("WITH ")
    return sql


def ident(number):
    return str(uuid.UUID(int=number))


def literal(value):
    if value is None:
        return "NULL"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    return "'" + str(value).replace("'", "''") + "'"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--container", required=True)
    parser.add_argument("--task", required=True)
    parser.add_argument("--database", required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--baseline", help="Exact local Git commit for equivalence and performance comparison")
    parser.add_argument("--scale", type=int, default=20000)
    parser.add_argument("--timeout-ms", type=int, default=10000)
    parser.add_argument("--worker-rows", type=int, default=3000)
    parser.add_argument("--artifact-rows", type=int, default=2000)
    parser.add_argument("--cache-rows", type=int, default=2000)
    parser.add_argument("--active-worker-modulo", type=int, default=5, help="0 produces no active workers")
    parser.add_argument("--settings-json", type=pathlib.Path, help="Bounded read-only pg_settings evidence to reproduce locally")
    args = parser.parse_args()
    allowed_settings = {"jit", "work_mem", "hash_mem_multiplier", "max_parallel_workers_per_gather", "effective_cache_size", "random_page_cost", "seq_page_cost", "cpu_tuple_cost"}
    settings = {}
    if args.settings_json:
        evidence = json.loads(args.settings_json.read_text())
        for row in evidence["plan"]["data"]:
            if row["name"] in allowed_settings:
                settings[row["name"]] = str(row["setting"]) + (row["unit"] or "")
                if row["unit"] == "8kB":
                    settings[row["name"]] = str(int(row["setting"]) * 8) + "kB"
    settings_sql = "".join(f"SET {name}={literal(value)};" for name, value in settings.items())
    assert all(1 <= value <= 100000 for value in [args.worker_rows, args.artifact_rows, args.cache_rows])
    assert 0 <= args.active_worker_modulo <= 100000
    assert re.fullmatch(r"worker_retention_[a-z0-9_]+", args.database), "dedicated fixture database required"
    assert 1 <= args.scale <= 1_000_000 and 1 <= args.timeout_ms <= 60_000
    inspected = json.loads(run(["docker", "inspect", args.container]).stdout)[0]
    labels = inspected["Config"]["Labels"]
    assert labels.get("io.tiangong.workspace.task") == args.task, "container owner mismatch"
    assert labels.get("io.tiangong.workspace.classification") == "disposable-test"
    assert labels.get("io.tiangong.workspace.retain") == "false"
    args.output.mkdir(parents=True, exist_ok=False)
    run(["docker", "exec", args.container, "createdb", "-U", "supabase_admin", args.database])
    command = ["docker", "exec", "-i", args.container, "psql", "-XqAt", "-v", "ON_ERROR_STOP=1", "-U", "supabase_admin", "-d", args.database]

    def execute(sql, *, check=True):
        return run(command, sql=settings_sql+"SET statement_timeout='60000ms'; SET lock_timeout='1000ms';"+sql, check=check)

    ddl = """
CREATE SCHEMA private;
CREATE TABLE private.worker_jobs(id uuid PRIMARY KEY,status text,payload_json jsonb,finished_at timestamptz,updated_at timestamptz,created_at timestamptz);
CREATE TABLE private.lca_package_artifacts(id uuid PRIMARY KEY,worker_job_id uuid,job_id uuid,is_pinned boolean,status text,expires_at timestamptz,artifact_byte_size bigint,created_at timestamptz DEFAULT '2026-08-01');
CREATE TABLE private.lca_package_request_cache(id uuid PRIMARY KEY,worker_job_id uuid,job_id uuid,status text,last_accessed_at timestamptz,export_artifact_id uuid,report_artifact_id uuid,hit_count bigint);
CREATE TABLE private.lca_package_export_items(id uuid PRIMARY KEY,worker_job_id uuid,job_id uuid,created_at timestamptz);
-- Match the deployed query-relevant indexes; no generic worker(status) index.
CREATE INDEX lca_package_artifacts_worker_job_idx ON private.lca_package_artifacts(worker_job_id) WHERE worker_job_id IS NOT NULL;
CREATE INDEX lca_package_artifacts_job_created_idx ON private.lca_package_artifacts(job_id,created_at DESC);
CREATE INDEX lca_package_artifacts_status_created_idx ON private.lca_package_artifacts(status,created_at DESC);
CREATE INDEX lca_package_request_cache_worker_job_idx ON private.lca_package_request_cache(worker_job_id) WHERE worker_job_id IS NOT NULL;
CREATE UNIQUE INDEX lca_package_request_cache_job_uidx ON private.lca_package_request_cache(job_id) WHERE job_id IS NOT NULL;
CREATE INDEX lca_package_request_cache_export_artifact_idx ON private.lca_package_request_cache(export_artifact_id) WHERE export_artifact_id IS NOT NULL;
CREATE INDEX lca_package_request_cache_report_artifact_idx ON private.lca_package_request_cache(report_artifact_id) WHERE report_artifact_id IS NOT NULL;
CREATE INDEX lca_package_request_cache_last_accessed_idx ON private.lca_package_request_cache(last_accessed_at DESC);
CREATE INDEX lca_package_export_items_gc_candidate_idx ON private.lca_package_export_items(created_at,id) INCLUDE(worker_job_id,job_id);
"""
    # Expected reasons are authored per case; the oracle does not reimplement SQL.
    expected = collections.defaultdict(lambda: [0, 0, 0])
    statements = [ddl]

    def insert(table, values, reason=None, *, byte_size=0, hits=0):
        statements.append(f"INSERT INTO private.{table} VALUES ({','.join(map(literal, values))});")
        if reason:
            totals = expected[(table, reason)]
            totals[0] += 1
            totals[1] += byte_size
            totals[2] += hits

    for i, status in [(1, "running"), (2, "queued"), (3, "waiting"), (4, "succeeded")]:
        insert("worker_jobs", [ident(i), status, json.dumps({"job_id": ident(100+i)}), RECENT if i == 4 else OLD, OLD, OLD])
    artifact_cases = [
        (10, None, None, True, "deleted", OLD, "protected_pinned_artifact"),
        (11, None, None, False, "deleted", OLD, "protected_already_deleted"),
        (12, None, None, False, "failed", None, "protected_artifact_not_ready"),
        (13, None, None, False, "ready", None, "protected_missing_expires_at"),
        (14, None, None, False, "ready", "2026-10-05", "protected_expires_at_in_future"),
        (15, 1, None, False, "ready", OLD, "protected_active_parent_worker_job"),
        (16, None, 102, False, "ready", OLD, "protected_active_parent_worker_job"),
        (17, None, None, False, "ready", OLD, "protected_request_cache_reference"),
        (18, None, None, False, "ready", OLD, "protected_request_cache_reference"),
        (19, None, None, False, "ready", AS_OF, "eligible_expired_unpinned_artifact"),
        (20, 4, 104, False, "ready", OLD, "eligible_expired_unpinned_artifact"),
        (21, None, None, False, "ready", OLD, "eligible_expired_unpinned_artifact"),
    ]
    for i, worker, job, pin, status, expires, reason in artifact_cases:
        insert("lca_package_artifacts", [ident(i), ident(worker) if worker else None, ident(job) if job else None, pin, status, expires, i], reason, byte_size=i)
    cache_cases = [
        (30, None, None, "pending", OLD, 17, None, "protected_active_request_cache"),
        (31, None, None, "running", OLD, None, None, "protected_active_request_cache"),
        (32, None, None, "ready", RECENT, None, 18, "protected_recent_request_cache_access"),
        (33, 2, None, "ready", OLD, None, None, "protected_active_parent_worker_job"),
        (34, None, 103, "ready", OLD, None, None, "protected_active_parent_worker_job"),
        (35, None, None, "ready", OLD, 20, None, "protected_live_artifact_reference"),
        (36, None, None, "ready", OLD, None, 12, "protected_live_artifact_reference"),
        (37, None, None, "ready", OLD, 11, None, "eligible_stale_request_cache"),
        (38, None, None, "ready", OLD, None, None, "eligible_stale_request_cache"),
        (39, 40, 140, "pending", OLD, None, None, "protected_active_request_cache"),
        (41, 42, 142, "ready", RECENT, None, None, "protected_recent_request_cache_access"),
    ]
    for i, worker, job, status, accessed, export, report, reason in cache_cases:
        insert("lca_package_request_cache", [ident(i), ident(worker) if worker else None, ident(job) if job else None, status, accessed, ident(export) if export else None, ident(report) if report else None, i], reason, hits=i)
    export_cases = [
        (50, 1, None, OLD, "protected_active_parent_worker_job"),
        (51, None, 103, OLD, "protected_active_parent_worker_job"),
        (52, 4, None, OLD, "protected_live_artifact_reference"),
        (53, None, 104, OLD, "protected_live_artifact_reference"),
        (54, 40, None, OLD, "protected_request_cache_reference"),
        (55, None, 142, OLD, "protected_request_cache_reference"),
        (56, None, None, RECENT, "protected_recent_export_item"),
        (57, None, None, OLD, "eligible_export_item_after_object_gc"),
        (58, 999, 999, OLD, "eligible_export_item_after_object_gc"),
    ]
    for i, worker, job, created, reason in export_cases:
        insert("lca_package_export_items", [ident(i), ident(worker) if worker else None, ident(job) if job else None, created], reason)
    # A recent canonical timestamp overrides old detail creation even without artifacts.
    insert("worker_jobs", [ident(6), "succeeded", '{}', RECENT, OLD, OLD])
    insert("worker_jobs", [ident(7), "succeeded", '{}', None, RECENT, OLD])
    insert("worker_jobs", [ident(8), "succeeded", '{}', None, None, RECENT])
    # Malformed/missing legacy payload identities remain harmless text comparisons.
    insert("worker_jobs", [ident(9), "running", '{"job_id":"not-a-uuid"}', OLD, OLD, OLD])
    insert("worker_jobs", [ident(99), "waiting", '{}', OLD, OLD, OLD])
    for i, worker in [(59, 6), (60, 7), (61, 8)]:
        insert("lca_package_export_items", [ident(i), ident(worker), None, OLD], "protected_recent_export_item")
    # Distinct legacy identities sharing a missing canonical parent must stay distinct.
    insert("lca_package_artifacts", [ident(22), None, ident(201), False, "ready", OLD, 22], "eligible_expired_unpinned_artifact", byte_size=22)
    for i, job, reason in [(64, 201, "protected_live_artifact_reference"), (65, 202, "eligible_export_item_after_object_gc")]:
        insert("lca_package_export_items", [ident(i), ident(999), ident(job), OLD], reason)
    # Multiple rows in one group and null recency retain exact counts.
    insert("lca_package_export_items", [ident(62), None, None, OLD], "eligible_export_item_after_object_gc")
    insert("lca_package_export_items", [ident(63), None, None, None], "eligible_export_item_after_object_gc")
    execute("\n".join(statements))
    variants = {"candidate": query((ROOT / SOURCE).read_text())}
    if args.baseline:
        assert re.fullmatch(r"[0-9a-f]{40}", args.baseline), "full exact baseline SHA required"
        variants["baseline"] = query(run(["git", "-C", str(ROOT), "show", f"{args.baseline}:{SOURCE}"]).stdout)
    for name, sql in variants.items():
        result = execute(f"SELECT json_agg(t) FROM ({sql}) t;").stdout
        rows = json.loads(result)
        actual = {(r["retention_area"], r["reason"]): [r["row_count"], r["total_artifact_bytes"], r["total_hit_count"]] for r in rows}
        assert actual == dict(expected), (name, actual, dict(expected))
        for row in rows:
            assert row["is_eligible"] == row["reason"].startswith("eligible_")
            assert row["retention_action"] == {
                "lca_package_artifacts": "delete_object_then_mark_deleted",
                "lca_package_request_cache": "delete_stale_request_cache_row",
                "lca_package_export_items": "delete_export_item_after_object_gc",
            }[row["retention_area"]]
        (args.output / f"{name}-boundary.json").write_text(result)
        (args.output / f"{name}.sql").write_text(sql)
    seed = (ROOT / "scripts/fixtures/package_retention_scale.sql").read_text().replace("__EXPORT_ROWS__", str(args.scale))
    for marker, value in [("__WORKER_ROWS__", args.worker_rows), ("__ARTIFACT_ROWS__", args.artifact_rows), ("__CACHE_ROWS__", args.cache_rows), ("__ACTIVE_MODULO__", args.active_worker_modulo)]:
        seed = seed.replace(marker, str(value))
    execute(seed)
    report = {"container_id": inspected["Id"], "task": args.task, "database": args.database, "scale": args.scale, "boundary_cases_passed": True, "baseline": args.baseline, "settings": settings, "worker_rows": args.worker_rows, "artifact_rows": args.artifact_rows, "cache_rows": args.cache_rows, "active_worker_modulo": args.active_worker_modulo, "plans": {}}
    for name, sql in variants.items():
        result = execute(f"SET statement_timeout='{args.timeout_ms}ms'; EXPLAIN (ANALYZE,BUFFERS,SETTINGS,FORMAT JSON) {sql};", check=False)
        (args.output / f"{name}-plan.json").write_text(result.stdout)
        (args.output / f"{name}-stderr.txt").write_text(result.stderr)
        if result.returncode:
            assert name == "baseline" and "statement timeout" in result.stderr, result.stderr
            report["plans"][name] = {"statement_timeout_ms": args.timeout_ms}
        else:
            plan = json.loads(result.stdout)[0]
            report["plans"][name] = {"execution_ms": plan["Execution Time"], "shared_hits": plan["Plan"]["Shared Hit Blocks"], "temp_written_blocks": plan["Plan"]["Temp Written Blocks"], "total_cost": plan["Plan"]["Total Cost"]}
    # Same full summary at representative scale (when baseline fits its bounded timeout).
    candidate = execute(f"SELECT json_agg(t) FROM ({variants['candidate']}) t;").stdout
    (args.output / "candidate-scale.json").write_text(candidate)
    if "baseline" in variants and "execution_ms" in report["plans"]["baseline"]:
        baseline = execute(f"SET statement_timeout='{args.timeout_ms}ms'; SELECT json_agg(t) FROM ({variants['baseline']}) t;").stdout
        assert json.loads(candidate) == json.loads(baseline), "scale equivalence"
        report["scale_equivalence_passed"] = True
    (args.output / "report.json").write_text(json.dumps(report, indent=2)+"\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
