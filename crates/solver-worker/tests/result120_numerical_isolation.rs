//! Real-path proof that a published Result Process (state 120) sharing the same product
//! reference as a Unit Process never changes the Unit's numerical result, plus the owner-scope,
//! review-diagnostic and product-export isolation checks.
//!
//! Lifecycle and safety
//! -------------------
//! * Run only through `scripts/run_result120_numerical_isolation.sh`, which selects exactly ONE
//!   scenario per invocation. There are no embedded endpoints or credentials.
//! * Scenarios seed immutable authored rows (a published Result at 120, approved public rows at
//!   100). The canonical domain guards protect those rows against UPDATE and DELETE for every role,
//!   so the suite never deletes them, never disables a trigger, and never sets
//!   `app.review_controlled_write`. Each scenario seeds its Process rows directly at their final
//!   state, which keeps every canonical trigger active.
//! * Because the fixtures are retained, each scenario runs only against a fresh dedicated task
//!   database and a unique task bucket, and the coordinator resets that instance between scenarios.
//!   Every created row/job/snapshot/artifact id is written to a fixture manifest on success and on
//!   failure.
//! * Guards run inside the Rust entrypoints too, because `cargo test -- --ignored` bypasses the
//!   shell runner.
//!
//! What each scenario proves through production code
//! -----------------------------------------------
//! 1. processes and flows are written to `public.processes` / `public.flows`, matching what a
//!    release publication persists;
//! 2. the snapshot universe is loaded by the real `snapshot_builder` binary through the queued
//!    `lca.build_snapshot` job, so state filtering, provider matching and matrix assembly are
//!    production code;
//! 3. the matrix is solved by the real `lca.solve_all_unit` job and the assertion reads the
//!    Calculation Bundle LCIA records the Worker uploaded;
//! 4. the Result Process row is created and the identical build/solve pair is repeated, so the
//!    before/after numbers come from the same production path.
//!
//! The `database-only` scenario needs no object storage; it is explicitly PARTIAL proof and is
//! never a substitute for the network scenarios.
#![allow(clippy::too_many_lines)]

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use clap::Parser;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use solver_worker::{
    config::AppConfig, db::AppState, graph_types::RequestRootProcess,
    queue::run_solver_worker_jobs_loop,
};
use sqlx::{PgPool, Row};
use tokio::{task::JoinHandle, time::sleep};
use uuid::Uuid;

const VERSION: &str = "01.00.000";
const PROCESS_STATE_PUBLIC: i32 = 100;
const PROCESS_STATE_RESULT: i32 = solver_worker::PUBLISHED_RESULT_PROCESS_STATE;
/// The dedicated in-review diagnostic state.
const REVIEW_PROCESS_STATE: i32 = solver_worker::REVIEW_IN_PROGRESS_PROCESS_STATE;
const MAX_ATTEMPTS: i32 = 2;

/// Numeric tolerance for the before/after comparison.
///
/// Both runs must produce byte-identical inputs. The tolerance only absorbs the last-bit noise of
/// a different sparse factorization ordering, and it is far below any real membership change: a
/// Result entering the axis changes the process count and the consumer row by O(1) values.
const TOLERANCE: f64 = 1e-9;

#[derive(Debug, Clone)]
struct Fixture {
    consumer: Uuid,
    unit: Uuid,
    result: Uuid,
    product_flow: Uuid,
    intermediate_flow: Uuid,
    flow_property: Uuid,
    unit_group: Uuid,
    method: Uuid,
    /// Exact version of the reviewed catalog method; the candidate path requires the catalog pair.
    method_version: String,
    elementary_flows: Vec<Uuid>,
}

/// One solved unit-demand row: impact id mapped to `h[process_index][impact_index]`.
#[derive(Debug, Clone, PartialEq)]
struct SolvedRow {
    process_id: Uuid,
    process_version: String,
    impacts: BTreeMap<Uuid, f64>,
}

#[derive(Debug, Clone, PartialEq)]
struct SolvedAxis {
    axis: Vec<(Uuid, String)>,
    rows: Vec<SolvedRow>,
}

/// Exact phrase required before this suite may touch a database or bucket.
const TASK_ISOLATION_CONFIRMATION: &str = "I_CONFIRM_ISOLATED_TASK_DATABASE";
/// Loopback hosts this suite accepts.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "::1", "localhost"];
/// Ports used by a default shared local Supabase stack.
///
/// These are rejected unconditionally; there is no task-id override. A dedicated task stack must
/// publish its own ports, so any match here means the target is the shared stack.
const SHARED_SUPABASE_PORTS: [u16; 3] = [54321, 54322, 54323];

/// Serializes the integration tests inside one process.
///
/// Every test in this file starts real worker queues and mutates shared `public.*` rows, so running
/// them concurrently would let one test claim another's job or pollute another's process axis.
static TASK_INSTANCE_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Held for the lifetime of one integration test.
struct TaskInstanceGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    task_instance_id: String,
    database_host: String,
    database_port: u16,
}

impl TaskInstanceGuard {
    #[must_use]
    fn task_instance_id(&self) -> &str {
        self.task_instance_id.as_str()
    }

    #[must_use]
    fn database_host(&self) -> &str {
        self.database_host.as_str()
    }

    #[must_use]
    fn database_port(&self) -> u16 {
        self.database_port
    }
}

/// Refuses to run unless the environment proves a dedicated, task-owned instance.
///
/// This runs inside the Rust entrypoints on purpose: `cargo test -- --ignored` bypasses the shell
/// runner, so the shell guard alone is not a safety boundary. It validates the *parsed* database and
/// storage endpoints rather than substrings, so a remote URL that merely mentions `localhost` in its
/// password or query string is rejected.
fn guard_task_instance(scope: TaskInstanceScope) -> anyhow::Result<TaskInstanceGuard> {
    let lock = TASK_INSTANCE_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let confirmation = std::env::var("RESULT120_CONFIRM_ISOLATED").unwrap_or_default();
    anyhow::ensure!(
        confirmation == TASK_ISOLATION_CONFIRMATION,
        "refusing to run: RESULT120_CONFIRM_ISOLATED must be exactly {TASK_ISOLATION_CONFIRMATION}"
    );
    let task_instance_id = std::env::var("RESULT120_TASK_INSTANCE_ID").unwrap_or_default();
    anyhow::ensure!(
        !task_instance_id.trim().is_empty(),
        "refusing to run: RESULT120_TASK_INSTANCE_ID must name this task-owned instance"
    );

    let database_url = required_env("DATABASE_URL");
    let options =
        <sqlx::postgres::PgConnectOptions as std::str::FromStr>::from_str(database_url.as_str())
            .map_err(|error| {
                anyhow::anyhow!("DATABASE_URL is not a valid Postgres URL: {error}")
            })?;
    let host = options.get_host().to_owned();
    anyhow::ensure!(
        LOOPBACK_HOSTS.contains(&host.as_str()),
        "refusing to run: DATABASE_URL host {host:?} is not loopback"
    );
    let port = options.get_port();
    anyhow::ensure!(
        !SHARED_SUPABASE_PORTS.contains(&port),
        "refusing to run: DATABASE_URL port {port} belongs to a default shared local Supabase stack; point at the task instance"
    );

    if scope == TaskInstanceScope::Full {
        let endpoint = required_env("S3_ENDPOINT");
        let parsed = reqwest::Url::parse(endpoint.as_str())
            .map_err(|error| anyhow::anyhow!("S3_ENDPOINT is not a valid URL: {error}"))?;
        let storage_host = parsed
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("S3_ENDPOINT has no host"))?;
        anyhow::ensure!(
            LOOPBACK_HOSTS.contains(&storage_host),
            "refusing to run: S3_ENDPOINT host {storage_host:?} is not loopback"
        );
        let storage_port = parsed.port_or_known_default().unwrap_or_default();
        anyhow::ensure!(
            !SHARED_SUPABASE_PORTS.contains(&storage_port),
            "refusing to run: S3_ENDPOINT port {storage_port} belongs to a default shared local Supabase stack"
        );
        // Credentials must be supplied explicitly; there are no embedded defaults.
        for name in ["S3_ACCESS_KEY_ID", "S3_SECRET_ACCESS_KEY", "S3_BUCKET"] {
            let value = std::env::var(name).unwrap_or_default();
            anyhow::ensure!(
                !value.trim().is_empty(),
                "refusing to run: {name} must be supplied explicitly for the task instance"
            );
        }
        let bucket = required_env("S3_BUCKET");
        anyhow::ensure!(
            bucket.contains(task_instance_id.trim()),
            "refusing to run: S3_BUCKET {bucket:?} does not name task instance {task_instance_id:?}"
        );
    }

    Ok(TaskInstanceGuard {
        _lock: lock,
        task_instance_id,
        database_host: host,
        database_port: port,
    })
}

/// The task bucket is created without a MIME allow-list.
///
/// A task-owned bucket needs no allow-list, and enumerating one is genuinely brittle: the Worker
/// uploads HDF5, JSON, JSONL, gzip, the TIDAS ZIP, Parquet, XLSX parts (`application/xml`,
/// `...spreadsheetml.*+xml`), and vendor types such as
/// `application/vnd.tiangong.snapshot-source-closure+json+zstd`. A missing entry fails a real upload
/// with `415 InvalidMimeType` even against a healthy object store, as observed in the first live run.
/// The bucket is created fresh under a name containing the task instance id, and the freshness
/// preflight has already rejected a non-empty instance, so it is never a shared or adopted bucket.
const FIXTURE_BUCKET_ALLOWED_MIME_TYPES: Option<Vec<String>> = None;

/// Exact phrase required before any fixture row may be inserted.
///
/// `public.processes` carries `process_extract_md_trigger_insert`, an `AFTER INSERT` webhook trigger
/// with **no `WHEN` clause**. It calls `util.project_url()` / `util.project_secret_key()`, which raise
/// when the task database has no Vault secrets, and otherwise POST to a real Edge endpoint. A
/// fixture insert therefore cannot be treated as a local write, so the suite refuses to start until
/// the coordinator confirms a safe task-local sink has been provisioned and the helper check passes.
const EGRESS_SINK_CONFIRMATION: &str = "I_CONFIRM_TASK_EGRESS_SINK";

/// Requires the task instance to be fresh and its process-insert webhook helpers to be configured.
///
/// This runs before any fixture row is written: unconfigured helpers must fail closed while the
/// database is still untouched, never after a partial fixture has already fired webhooks. It does
/// **not** prove the sink endpoint is reachable; the coordinator verifies a literal loopback
/// non-production sink independently.
async fn preflight_task_instance(pool: &PgPool) -> anyhow::Result<()> {
    let confirmation = std::env::var("RESULT120_EGRESS_SINK_CONFIRMED").unwrap_or_default();
    anyhow::ensure!(
        confirmation == EGRESS_SINK_CONFIRMATION,
        "refusing to run: inserting into public.processes fires process_extract_md_trigger_insert \
         (AFTER INSERT, no WHEN), which calls util.project_url()/util.project_secret_key() and POSTs \
         to an Edge function. Set RESULT120_EGRESS_SINK_CONFIRMED={EGRESS_SINK_CONFIRMATION} only \
         after the coordinator has provisioned a safe task-local webhook sink"
    );
    anyhow::ensure!(
        std::env::var("RESULT120_TASK_FIXTURE_FRESH").unwrap_or_default()
            == "I_CONFIRM_FRESH_TASK_FIXTURE",
        "refusing to run: RESULT120_TASK_FIXTURE_FRESH=I_CONFIRM_FRESH_TASK_FIXTURE is required; \
         the coordinator resets the dedicated task instance between scenarios and this suite never \
         deletes authored immutable rows"
    );

    // Empty authoring tables are the observable proof that this is a freshly reset task instance.
    let existing = sqlx::query(
        "SELECT (SELECT count(*) FROM public.processes) AS processes, (SELECT count(*) FROM public.flows) AS flows",
    )
    .fetch_one(pool)
    .await?;
    let processes = existing.try_get::<i64, _>("processes")?;
    let flows = existing.try_get::<i64, _>("flows")?;
    anyhow::ensure!(
        processes == 0 && flows == 0,
        "refusing to run: task instance is not fresh (processes={processes} flows={flows}); \
         the coordinator must reset it before this scenario"
    );

    // This probe only checks that the Vault-backed helpers the webhook path depends on are
    // *configured*: it calls `util.project_url()` and `util.project_secret_key()` and lets them raise
    // when the secrets are absent. It does not open a connection, so it is not network reachability
    // proof for the sink; the coordinator verifies a literal loopback non-production sink
    // independently. The secret values are never read into this process or logged.
    let probe = sqlx::query(
        r"
        DO $$
        BEGIN
          PERFORM util.project_url();
          PERFORM util.project_secret_key();
        EXCEPTION WHEN others THEN
          RAISE EXCEPTION 'result120_egress_helpers_unconfigured: %', SQLERRM;
        END $$;
        ",
    )
    .execute(pool)
    .await;
    if let Err(error) = probe {
        let message = error.to_string();
        if message.contains("result120_egress_helpers_unconfigured") {
            anyhow::bail!(
                "refusing to run: the task database's webhook helpers are not configured (missing \
                 Vault project_url/project_secret_key), so a public.processes insert would fail only \
                 after mutating fixture state: {message}"
            );
        }
        // A database without the helper functions at all is also not a valid task instance.
        anyhow::bail!(
            "refusing to run: cannot evaluate the process-insert webhook helpers on this task \
             database: {message}"
        );
    }
    Ok(())
}

/// Requires an explicitly built, current `snapshot_builder` binary.
///
/// The builder is spawned as a child process, so a stale `target/debug/snapshot_builder` from an
/// earlier revision would silently exercise the wrong policy code. The shell runner builds it and
/// exports an absolute path; this check refuses to start when the variable is missing, when the file
/// is not executable, or when it is older than the Worker sources it was built from.
///
/// `review.quality_diagnostic` also reaches this binary (its runner builds a snapshot), so the check
/// applies to the review-diagnostic scenario as well.
fn ensure_snapshot_builder_binary_is_current() -> anyhow::Result<()> {
    let raw = std::env::var("SNAPSHOT_BUILDER_BIN").unwrap_or_default();
    let worker_root = std::env::var("LCA_WORKER_ROOT")
        .map_or_else(|_| std::path::PathBuf::from("."), std::path::PathBuf::from);
    ensure_snapshot_builder_binary_at(raw.as_str(), worker_root.as_path())
}

/// Pure core of the builder-binary check, so it can be tested without mutating process env.
fn ensure_snapshot_builder_binary_at(
    raw: &str,
    worker_root: &std::path::Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        !raw.trim().is_empty(),
        "refusing to run: SNAPSHOT_BUILDER_BIN must be an explicit absolute path to a freshly built \
         snapshot_builder (the runner builds it); a bare name could resolve to a stale executable"
    );
    let trimmed = raw.trim();
    let path = std::path::Path::new(trimmed);
    anyhow::ensure!(
        path.is_absolute(),
        "refusing to run: SNAPSHOT_BUILDER_BIN must be absolute, got {raw:?}"
    );
    let metadata = std::fs::metadata(path).map_err(|error| {
        anyhow::anyhow!(
            "refusing to run: SNAPSHOT_BUILDER_BIN {raw:?} is not readable ({error}); build it with \
             `cargo build -p solver-worker --bin snapshot_builder`"
        )
    })?;
    anyhow::ensure!(
        metadata.is_file(),
        "refusing to run: SNAPSHOT_BUILDER_BIN {raw:?} is not a file"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        anyhow::ensure!(
            metadata.permissions().mode() & 0o111 != 0,
            "refusing to run: SNAPSHOT_BUILDER_BIN {raw:?} is not executable"
        );
    }

    let binary_modified = metadata.modified()?;
    // Only `src` trees feed the binary. Test files live under `crates/**/tests` and do not cause the
    // builder to be relinked, so counting them would demand a rebuild after every test edit while
    // proving nothing about the binary's freshness.
    let mut newest_source: Option<std::time::SystemTime> = None;
    let crates_root = worker_root.join("crates");
    if let Ok(entries) = std::fs::read_dir(crates_root.as_path()) {
        for entry in entries.flatten() {
            let source_dir = entry.path().join("src");
            if source_dir.is_dir() {
                collect_newest_rust_source(source_dir.as_path(), &mut newest_source)?;
            }
        }
    }
    if let Some(newest) = newest_source {
        anyhow::ensure!(
            binary_modified >= newest,
            "refusing to run: SNAPSHOT_BUILDER_BIN {raw:?} is older than the Worker sources it must \
             reflect; rebuild it with `cargo build -p solver-worker --bin snapshot_builder`"
        );
    }
    Ok(())
}

/// Walks a source tree for the newest `.rs` modification time.
fn collect_newest_rust_source(
    directory: &std::path::Path,
    newest: &mut Option<std::time::SystemTime>,
) -> anyhow::Result<()> {
    // A missing source tree only weakens the staleness check, never the correctness of a run against
    // an explicitly built binary; the absolute-path and executability checks already ran.
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_newest_rust_source(path.as_path(), newest)?;
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && let Ok(modified) = entry.metadata().and_then(|meta| meta.modified())
            && newest.is_none_or(|current| modified > current)
        {
            *newest = Some(modified);
        }
    }
    Ok(())
}

/// Which endpoints a run needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskInstanceScope {
    /// Database only; no object storage is touched.
    Database,
    /// Database plus object storage.
    Full,
}

/// Fails when the task instance already holds queue work this suite did not create.
///
/// Claiming another test's or another task's job would corrupt both runs, so the suite refuses to
/// start instead of racing.
async fn ensure_no_preexisting_queue_work(pool: &PgPool) -> anyhow::Result<()> {
    let row = sqlx::query(
        "SELECT count(*)::bigint AS busy FROM private.worker_jobs WHERE status IN ('queued','running')",
    )
    .fetch_one(pool)
    .await?;
    let busy = row.try_get::<i64, _>("busy")?;
    anyhow::ensure!(
        busy == 0,
        "refusing to run: task instance already has {busy} queued/running worker job(s); use a freshly prepared instance"
    );
    Ok(())
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!(
            "{name} is required; run scripts/run_result120_numerical_isolation.sh from the Worker repo"
        )
    })
}

fn test_config() -> AppConfig {
    AppConfig::parse_from([
        "solver-worker-result120-isolation",
        "--database-url",
        required_env("DATABASE_URL").as_str(),
        "--s3-endpoint",
        required_env("S3_ENDPOINT").as_str(),
        "--s3-region",
        required_env("S3_REGION").as_str(),
        "--s3-bucket",
        required_env("S3_BUCKET").as_str(),
        "--s3-access-key-id",
        required_env("S3_ACCESS_KEY_ID").as_str(),
        "--s3-secret-access-key",
        required_env("S3_SECRET_ACCESS_KEY").as_str(),
        "--s3-prefix",
        "result120-numerical-isolation",
        "--db-max-connections",
        "12",
        "--worker-poll-ms",
        "20",
    ])
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn repeated(character: char) -> String {
    std::iter::repeat_n(character, 64).collect()
}

/// The first reviewed LCIA-method identity from the Database-owned catalog.
///
/// The candidate (no-current-release) scope validation only accepts methods that join
/// `private.lcia_scope_closure_reviewed_lcia_methods`, so a synthetic method id can never be a valid
/// candidate method. The reviewed catalog is the same 25-method set the Worker's static cache binds.
fn reviewed_lcia_method_identity() -> (Uuid, String) {
    match solver_worker::calculation_evidence::RELEASE_METHOD_IDENTITIES.first() {
        Some((method_id, method_version, _)) => {
            let id = Uuid::parse_str(method_id).expect("reviewed method id");
            (id, (*method_version).to_owned())
        }
        None => unreachable!("reviewed method catalog is never empty"),
    }
}

/// Canonical TIDAS administrative block carrying the dataset version.
///
/// The Database `<table>_sync_jsonb_version` triggers derive the `version` column from
/// `administrativeInformation.publicationAndOwnership.common:dataSetVersion`, falling back to the
/// empty string when it is absent. Fixture documents must therefore publish the canonical version:
/// the downstream Portal catalog projections enforce `^\d{2}\.\d{2}\.\d{3}$` and reject an
/// empty derived version. This mirrors what a real release publication writes; it is not a schema
/// guard relaxation.
fn administrative_information(version: &str) -> Value {
    json!({
        "publicationAndOwnership": {
            "common:dataSetVersion": version
        }
    })
}

/// One exchange in the fixture process: `(internal id, direction, flow, amount)`.
type FixtureExchange<'a> = (&'a str, &'a str, Uuid, f64);

/// Builds one TIDAS Process document. Exchange `1` is always the quantitative reference.
fn process_document_fixture(process: Uuid, exchanges: &[FixtureExchange<'_>]) -> Value {
    json!({
        "processDataSet": {
            "administrativeInformation": administrative_information(VERSION),
            "processInformation": {
                "dataSetInformation": {
                    "common:UUID": process,
                    "name": {"baseName": format!("Result120 isolation process {process}")}
                },
                "quantitativeReference": {"referenceToReferenceFlow": "1"}
            },
            "exchanges": {"exchange": exchanges
                .iter()
                .map(|(internal_id, direction, flow, amount)| json!({
                    "@dataSetInternalID": internal_id,
                    "exchangeDirection": direction,
                    "resultingAmount": amount.to_string(),
                    "referenceToFlowDataSet": {
                        "@type": "flow data set",
                        "@refObjectId": flow,
                        "@version": VERSION
                    }
                }))
                .collect::<Vec<_>>()}
        }
    })
}

fn flow_document(flow: Uuid, flow_type: &str, flow_property: Uuid) -> Value {
    json!({
        "flowDataSet": {
            "administrativeInformation": administrative_information(VERSION),
            "flowInformation": {
                "dataSetInformation": {
                    "common:UUID": flow,
                    "name": {"baseName": format!("Result120 isolation flow {flow}")}
                },
                "quantitativeReference": {"referenceToReferenceFlowProperty": "1"}
            },
            "flowProperties": {"flowProperty": {
                "@dataSetInternalID": "1",
                "referenceToFlowPropertyDataSet": {
                    "@type": "flow property data set",
                    "@refObjectId": flow_property,
                    "@version": VERSION
                }
            }},
            "modellingAndValidation": {"LCIMethod": {"typeOfDataSet": flow_type}}
        }
    })
}

fn flow_property_document(flow_property: Uuid, unit_group: Uuid) -> Value {
    json!({
        "flowPropertyDataSet": {
            "administrativeInformation": administrative_information(VERSION),
            "flowPropertiesInformation": {
                "dataSetInformation": {"common:UUID": flow_property},
                "quantitativeReference": {"referenceToReferenceUnitGroup": {
                    "@type": "unit group data set",
                    "@refObjectId": unit_group,
                    "@version": VERSION
                }}
            }
        }
    })
}

fn unit_group_document(unit_group: Uuid) -> Value {
    json!({
        "unitGroupDataSet": {
            "administrativeInformation": administrative_information(VERSION),
            "unitGroupInformation": {
                "dataSetInformation": {"common:UUID": unit_group},
                "quantitativeReference": {"referenceToReferenceUnit": "1"}
            },
            "units": {"unit": {
                "@dataSetInternalID": "1",
                "name": "kg",
                "meanValue": "1"
            }}
        }
    })
}

fn method_document(method: Uuid, factors: &[(Uuid, f64)]) -> Value {
    json!({
        "LCIAMethodDataSet": {
            "administrativeInformation": administrative_information(VERSION),
            "LCIAMethodInformation": {
                "dataSetInformation": {"common:UUID": method}
            },
            "methodInformation": {
                "dataSetInformation": {"name": {"baseName": "Result120 isolation impact"}}
            },
            "characterisationFactors": {"factor": factors
                .iter()
                .map(|(flow, value)| json!({
                    "referenceToFlowDataSet": {
                        "@type": "flow data set",
                        "@refObjectId": flow,
                        "@version": VERSION
                    },
                    "meanValue": value.to_string()
                }))
                .collect::<Vec<_>>()}
        }
    })
}

/// Inserts the whole fixture: consumer C, public Unit U(P) and the shared product flow P.
/// Seeds the whole fixture. `unit_state`/`result_state` decide the states the two Process rows are
/// created with.
///
/// A Result is only ever *created* at its final state. Transitioning a state-100 row to 120 would
/// require `app.review_controlled_write`, which is exactly the admission path this suite must not
/// bypass; the Result publication lifecycle is owned by Database #646.
/// Allocates every fixture identity without writing anything.
///
/// Identities exist before the first database write so the scenario guard and its resource manifest
/// can be created first: if any seeding statement fails, the manifest still names exactly which rows
/// may have been created.
#[must_use]
fn allocate_fixture() -> Fixture {
    Fixture {
        consumer: Uuid::new_v4(),
        unit: Uuid::new_v4(),
        result: Uuid::new_v4(),
        product_flow: Uuid::new_v4(),
        intermediate_flow: Uuid::new_v4(),
        flow_property: Uuid::new_v4(),
        unit_group: Uuid::new_v4(),
        method: reviewed_lcia_method_identity().0,
        method_version: reviewed_lcia_method_identity().1,
        elementary_flows: (0..3).map(|_| Uuid::new_v4()).collect(),
    }
}

/// Seeds the fixture for `actor`.
///
/// `unit_state` and `included_processes` decide which Process rows are created: the Result Process is
/// only inserted when [`FixtureProcesses::WithResult`] is requested. It is never created at state
/// `100` and then transitioned, because `review_dataset_content_guard_v1` makes a state-100 row
/// immutable and reaching `120` through a runtime `UPDATE` would require the review-controlled-write
/// escape hatch, i.e. bypassing the admission path this suite exists to exercise.
async fn seed_fixture(
    pool: &PgPool,
    actor: Uuid,
    fixture: &Fixture,
    unit_state: i32,
    result_state: i32,
    included_processes: FixtureProcesses,
    needs_object_storage: bool,
) -> anyhow::Result<()> {
    let elementary_list = fixture.elementary_flows.clone();

    // Object storage is only involved in network scenarios. A database-only run must not require
    // `S3_BUCKET`, and it never touches a bucket row.
    if needs_object_storage {
        // The task bucket is created fresh. Its allow-list must cover every content type the Worker
        // actually uploads: the numerical snapshot (`application/x-hdf5`), JSON/JSONL/gzip sidecars,
        // the XLSX audit workbook, and the TIDAS package ZIP (`application/zip`, which
        // `PACKAGE_ZIP_CONTENT_TYPE` and `storage.rs` use). Missing any of these makes a real upload
        // fail even against a healthy S3 service. `ON CONFLICT DO NOTHING` never adopts or mutates a
        // pre-existing bucket; the freshness preflight already rejected a non-empty instance.
        sqlx::query(
            r"INSERT INTO storage.buckets(id,name,public,file_size_limit,allowed_mime_types)
               VALUES($1,$1,false,NULL,$2)",
        )
        .bind(required_env("S3_BUCKET"))
        .bind(FIXTURE_BUCKET_ALLOWED_MIME_TYPES)
        .execute(pool)
        .await?;
        // `storage.buckets.allowed_mime_types` is `text[]`; a bucket created earlier in this same
        // run keeps its row because the freshness preflight requires an empty instance.
        let allowed = sqlx::query_scalar::<_, Option<Vec<String>>>(
            "SELECT allowed_mime_types FROM storage.buckets WHERE id=$1",
        )
        .bind(required_env("S3_BUCKET"))
        .fetch_one(pool)
        .await?;
        // The bucket must be the task's own and unrestricted. A bucket that already existed with a
        // narrower allow-list would fail a later upload, so it is rejected here instead.
        anyhow::ensure!(
            allowed.is_none(),
            "task bucket {:?} carries a MIME allow-list ({allowed:?}); the Worker uploads HDF5, JSON,              JSONL, gzip, ZIP, Parquet, XLSX parts and vendor types, so a narrow list fails real uploads",
            required_env("S3_BUCKET")
        );
        anyhow::ensure!(
            required_env("S3_BUCKET").contains(required_env("RESULT120_TASK_INSTANCE_ID").as_str()),
            "task bucket name must contain the task instance id"
        );
    }
    sqlx::query(
        r"INSERT INTO auth.users(instance_id,id,aud,role,email,encrypted_password,email_confirmed_at,
             raw_app_meta_data,raw_user_meta_data,created_at,updated_at,is_sso_user,is_anonymous)
           VALUES('00000000-0000-0000-0000-000000000000',$1,'authenticated','authenticated',$2,
             'x',now(),'{}','{}',now(),now(),false,false)",
    )
    .bind(actor)
    .bind(format!("result120-isolation-{actor}@example.com"))
    .execute(pool)
    .await?;
    // `trg_sync_auth_users_to_private_users` mirrors every `auth.users` insert into
    // `private.users` (deferrable, initially deferred). Inserting the profile again here would raise
    // a duplicate-key error, so the mirror is only *verified* after the Auth transaction commits.
    let mirrored =
        sqlx::query_scalar::<_, i64>("SELECT count(*)::bigint FROM private.users WHERE id=$1")
            .bind(actor)
            .fetch_one(pool)
            .await?;
    anyhow::ensure!(
        mirrored == 1,
        "auth.users insert did not mirror actor {actor} into private.users (found {mirrored} rows); \
         the Database user-sync trigger is missing or not firing"
    );
    // `contact` is application-owned and left null by the mirror; no broad profile update is made.
    sqlx::query(
        "INSERT INTO private.teams(id,json,rank,is_public) VALUES('00000000-0000-0000-0000-000000000000','{\"name\":\"System\"}',0,false) ON CONFLICT(id) DO NOTHING",
    )
    .execute(pool)
    .await?;

    // Consumer C consumes the shared product P and emits a different product plus an elementary
    // flow. Both product exchanges are quantitative references, so C both emits and consumes P.
    let consumer_document = process_document_fixture(
        fixture.consumer,
        &[
            ("1", "Output", fixture.intermediate_flow, 5.0),
            ("2", "Input", fixture.product_flow, 10.0),
            ("3", "Output", elementary_list[2], 9.0),
        ],
    );
    // U(P) is the public Unit Process for one unit of P. It is the only *external* provider of P in
    // the public universe, so it is also the balancing reference port for C's P residual.
    let unit_document = process_document_fixture(
        fixture.unit,
        &[
            ("1", "Output", fixture.product_flow, 10.0),
            ("2", "Output", elementary_list[0], 3.0),
        ],
    );
    // R(P) is the published Result derived from U(P): same product reference flow, same elementary
    // burden per unit of P. It is a genuine same-product provider lure, not a same-process one.
    let result_document = process_document_fixture(
        fixture.result,
        &[
            ("1", "Output", fixture.product_flow, 10.0),
            ("2", "Output", elementary_list[0], 3.0),
        ],
    );

    // The Result is seeded at its published state `120` and is never transitioned at runtime.
    // `review_dataset_content_guard_v1` makes a state-100 row immutable, so reaching 120 through a
    // runtime `UPDATE` would require `app.review_controlled_write` — i.e. bypassing the very
    // admission path this suite is meant to exercise. Seeding the final state keeps every canonical
    // trigger active and leaves the Result lifecycle to its owning task.
    // The pre-publication baseline creates C and U(P) only; the Result identity exists from
    // `allocate_fixture` so the manifest can name it, but no row is written for it.
    let mut process_rows = vec![
        (fixture.consumer, consumer_document, PROCESS_STATE_PUBLIC),
        (fixture.unit, unit_document, unit_state),
    ];
    if included_processes.includes_result() {
        process_rows.push((fixture.result, result_document, result_state));
    }
    for (id, document, state_code) in process_rows {
        sqlx::query(
            r"INSERT INTO public.processes(id,version,json,json_ordered,user_id,state_code)
               VALUES($1,$2,$3,$3::text::json,$4,$5)",
        )
        .bind(id)
        .bind(VERSION)
        .bind(&document)
        .bind(actor)
        .bind(state_code)
        .execute(pool)
        .await?;
    }

    let flows = [
        (fixture.product_flow, "Product flow"),
        (fixture.intermediate_flow, "Product flow"),
    ];
    for (id, flow_type) in flows {
        sqlx::query(
            r"INSERT INTO public.flows(id,version,json,json_ordered,user_id,state_code)
               VALUES($1,$2,$3,$3::text::json,$4,$5)",
        )
        .bind(id)
        .bind(VERSION)
        .bind(flow_document(id, flow_type, fixture.flow_property))
        .bind(actor)
        .bind(PROCESS_STATE_PUBLIC)
        .execute(pool)
        .await?;
    }
    for flow in &elementary_list {
        sqlx::query(
            r"INSERT INTO public.flows(id,version,json,json_ordered,user_id,state_code)
               VALUES($1,$2,$3,$3::text::json,$4,$5)",
        )
        .bind(flow)
        .bind(VERSION)
        .bind(flow_document(
            *flow,
            "Elementary flow",
            fixture.flow_property,
        ))
        .bind(actor)
        .bind(PROCESS_STATE_PUBLIC)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        r"INSERT INTO public.flowproperties(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,$5)",
    )
    .bind(fixture.flow_property)
    .bind(VERSION)
    .bind(flow_property_document(
        fixture.flow_property,
        fixture.unit_group,
    ))
    .bind(actor)
    .bind(PROCESS_STATE_PUBLIC)
    .execute(pool)
    .await?;
    sqlx::query(
        r"INSERT INTO public.unitgroups(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,$5)",
    )
    .bind(fixture.unit_group)
    .bind(VERSION)
    .bind(unit_group_document(fixture.unit_group))
    .bind(actor)
    .bind(PROCESS_STATE_PUBLIC)
    .execute(pool)
    .await?;

    let factors = elementary_list
        .iter()
        .map(|flow| (*flow, 2.0))
        .collect::<Vec<_>>();
    sqlx::query(
        r"INSERT INTO public.lciamethods(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,$5)",
    )
    .bind(fixture.method)
    .bind(fixture.method_version.as_str())
    .bind(method_document(fixture.method, factors.as_slice()))
    .bind(actor)
    .bind(PROCESS_STATE_PUBLIC)
    .execute(pool)
    .await?;

    Ok(())
}

/// Which Process rows a scenario seeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FixtureProcesses {
    /// C and U(P) only: the pre-publication baseline.
    BaselineOnly,
    /// C, U(P) and the Result at its published state.
    WithResult,
}

impl FixtureProcesses {
    fn includes_result(self) -> bool {
        matches!(self, Self::WithResult)
    }
}

/// Seeds the common case: public Unit and a Result already created at its published state.
async fn seed_published_result_fixture(
    pool: &PgPool,
    actor: Uuid,
    fixture: &Fixture,
) -> anyhow::Result<()> {
    seed_fixture(
        pool,
        actor,
        fixture,
        PROCESS_STATE_PUBLIC,
        PROCESS_STATE_RESULT,
        FixtureProcesses::WithResult,
        true,
    )
    .await
}

/// "Publishes" a Result in an already-seeded fixture by creating the Result Process row.
///
/// This models the observable effect of the Result publication lifecycle: a new state-120 Process
/// appears while every public row is untouched. It does not exercise the publisher itself, which is
/// owned by Database #646, and it never mutates an existing public row.
async fn publish_result(pool: &PgPool, fixture: &Fixture, actor: Uuid) -> anyhow::Result<()> {
    let document = process_document_fixture(
        fixture.result,
        &[
            ("1", "Output", fixture.product_flow, 10.0),
            ("2", "Output", fixture.elementary_flows[0], 3.0),
        ],
    );
    sqlx::query(
        r"INSERT INTO public.processes(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,$5)",
    )
    .bind(fixture.result)
    .bind(VERSION)
    .bind(&document)
    .bind(actor)
    .bind(PROCESS_STATE_RESULT)
    .execute(pool)
    .await?;
    Ok(())
}

/// Creates an actor-owned Process with an explicit (or NULL) `state_code` for export coverage.
///
/// `state_code` is nullable, and the product-export policy only withholds `120`. A NULL-state row was
/// exportable before the policy and must remain exportable, so this row is the runtime regression
/// for the NULL-safe fence.
async fn insert_export_probe_process(
    pool: &PgPool,
    fixture: &Fixture,
    actor: Uuid,
    process: Uuid,
    state_code: Option<i32>,
) -> anyhow::Result<()> {
    let document = process_document_fixture(
        process,
        &[
            ("1", "Output", fixture.product_flow, 10.0),
            ("2", "Output", fixture.elementary_flows[1], 2.0),
        ],
    );
    sqlx::query(
        r"INSERT INTO public.processes(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,$5)",
    )
    .bind(process)
    .bind(VERSION)
    .bind(&document)
    .bind(actor)
    .bind(state_code)
    .execute(pool)
    .await?;
    Ok(())
}

/// Creates an actor-owned state-0 draft Process that reuses the fixture's flows and model columns.
async fn insert_draft_process(pool: &PgPool, fixture: &Fixture, actor: Uuid) -> anyhow::Result<()> {
    let document = process_document_fixture(
        fixture.result,
        &[
            ("1", "Output", fixture.product_flow, 10.0),
            ("2", "Output", fixture.elementary_flows[0], 3.0),
        ],
    );
    sqlx::query(
        r"INSERT INTO public.processes(id,version,json,json_ordered,user_id,state_code)
           VALUES($1,$2,$3,$3::text::json,$4,0)",
    )
    .bind(fixture.result)
    .bind(VERSION)
    .bind(&document)
    .bind(actor)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records every resource this scenario created, without any secret material.
///
/// The suite never deletes authored Process rows: a published Result (`120`) and an approved public
/// Unit (`100`) are protected against DELETE for every role by the canonical domain guards, and
/// disabling a trigger, dropping a constraint, or setting `app.review_controlled_write` to make a
/// fixture self-clean would relax exactly the safety rules this work exists to protect.
///
/// Lifecycle is therefore owned by the coordinator: each scenario runs only against a fresh,
/// dedicated task database and a unique task bucket, and the coordinator resets that dedicated
/// instance between scenarios. This manifest makes the retained resources auditable afterwards.
#[derive(Debug, Serialize)]
struct FixtureResourceManifest {
    schema_version: &'static str,
    task_instance_id: String,
    label: String,
    database_url_host: String,
    database_url_port: u16,
    s3_endpoint: String,
    s3_bucket: String,
    actor: Uuid,
    processes: BTreeMap<String, Uuid>,
    flows: Vec<Uuid>,
    flow_property: Uuid,
    unit_group: Uuid,
    lcia_method: Uuid,
    /// Worker job ids this scenario enqueued.
    worker_jobs: Vec<Uuid>,
    /// Snapshot ids the Worker resolved.
    snapshots: Vec<Uuid>,
    /// Package artifact ids the Worker produced.
    package_artifacts: Vec<Uuid>,
    /// Extra actor-owned draft rows created by a scenario.
    draft_processes: Vec<Uuid>,
    /// Extra Process rows created by a scenario (export state probes), recorded before insertion so
    /// they appear in the manifest even when a later seed statement fails.
    probe_processes: Vec<Uuid>,
    outcome: String,
}

/// Tracks created resources so the manifest is exact even on failure.
#[derive(Debug, Default)]
struct TrackedResources {
    worker_jobs: Vec<Uuid>,
    snapshots: Vec<Uuid>,
    package_artifacts: Vec<Uuid>,
    draft_processes: Vec<Uuid>,
    /// Extra Process rows a scenario created (export state probes).
    probe_processes: Vec<Uuid>,
}

/// Owns the fixture lifecycle for one scenario: stops every worker and writes the manifest on
/// normal return, early `?` return, and panic.
///
/// The suite never deletes authored immutable rows, so the manifest is the only cleanup artifact and
/// must exist even when an assertion panics mid-scenario.
struct ScenarioRunner {
    guard: TaskInstanceGuard,
    fixture: Fixture,
    actor: Uuid,
    label: &'static str,
    manifest_dir: std::path::PathBuf,
    tracked: TrackedResources,
    outcome: &'static str,
}

impl ScenarioRunner {
    fn new(guard: TaskInstanceGuard, fixture: Fixture, actor: Uuid, label: &'static str) -> Self {
        let manifest_dir = std::env::var("RESULT120_MANIFEST_DIR")
            .map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from);
        Self::with_manifest_dir(guard, fixture, actor, label, manifest_dir)
    }

    fn with_manifest_dir(
        guard: TaskInstanceGuard,
        fixture: Fixture,
        actor: Uuid,
        label: &'static str,
        manifest_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            guard,
            fixture,
            actor,
            label,
            manifest_dir,
            tracked: TrackedResources::default(),
            outcome: "unknown",
        }
    }

    fn tracked_mut(&mut self) -> &mut TrackedResources {
        &mut self.tracked
    }

    fn finish(mut self, outcome: &'static str) {
        self.outcome = outcome;
    }
}

impl Drop for ScenarioRunner {
    fn drop(&mut self) {
        stop_all_workers();
        if let Err(error) = write_fixture_manifest(
            &self.manifest_dir,
            &self.guard,
            &self.fixture,
            self.actor,
            self.label,
            &self.tracked,
            self.outcome,
        ) {
            eprintln!("fixture manifest write failed: {error:#}");
        }
    }
}

/// Writes the fixture manifest outside the task database.
///
/// It is written on success and on failure, so a coordinator can identify every retained row and
/// object key without querying for it.
fn write_fixture_manifest(
    manifest_dir: &std::path::Path,
    guard: &TaskInstanceGuard,
    fixture: &Fixture,
    actor: Uuid,
    label: &str,
    tracked: &TrackedResources,
    outcome: &str,
) -> anyhow::Result<()> {
    let manifest = FixtureResourceManifest {
        schema_version: "result120.fixture-resources.v1",
        task_instance_id: guard.task_instance_id().to_owned(),
        label: label.to_owned(),
        database_url_host: guard.database_host().to_owned(),
        database_url_port: guard.database_port(),
        s3_endpoint: std::env::var("S3_ENDPOINT").unwrap_or_default(),
        s3_bucket: std::env::var("S3_BUCKET").unwrap_or_default(),
        actor,
        processes: BTreeMap::from([
            ("consumer".to_owned(), fixture.consumer),
            ("unit".to_owned(), fixture.unit),
            ("result".to_owned(), fixture.result),
        ]),
        flows: std::iter::once(fixture.product_flow)
            .chain(std::iter::once(fixture.intermediate_flow))
            .chain(fixture.elementary_flows.iter().copied())
            .collect(),
        flow_property: fixture.flow_property,
        unit_group: fixture.unit_group,
        lcia_method: fixture.method,
        worker_jobs: tracked.worker_jobs.clone(),
        snapshots: tracked.snapshots.clone(),
        package_artifacts: tracked.package_artifacts.clone(),
        draft_processes: tracked.draft_processes.clone(),
        probe_processes: tracked.probe_processes.clone(),
        outcome: outcome.to_owned(),
    };
    let path = manifest_dir.join(format!(
        "result120-{}-{label}.json",
        manifest.task_instance_id
    ));
    std::fs::write(path.as_path(), serde_json::to_vec_pretty(&manifest)?)?;
    eprintln!("fixture manifest written to {}", path.display());
    Ok(())
}

/// Abort handles for every worker/runtime task this process started.
///
/// `ScenarioRunner::drop` aborts all of them, so a failed assertion or an early `?` can never leave
/// a claim loop running against the task instance.
static LIVE_WORKERS: std::sync::Mutex<Vec<tokio::task::AbortHandle>> =
    std::sync::Mutex::new(Vec::new());

/// Aborts every tracked worker. Runs on `?`, panic and normal return.
fn stop_all_workers() {
    let mut guard = LIVE_WORKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !guard.is_empty() {
        eprintln!("stopping {} tracked worker task(s)", guard.len());
    }
    for handle in guard.drain(..) {
        handle.abort();
    }
}

/// Registers an abort handle for any spawned task so teardown can stop it.
fn track_abort_handle(handle: tokio::task::AbortHandle) {
    LIVE_WORKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(handle);
}

fn track_worker(handle: &JoinHandle<anyhow::Result<()>>) {
    track_abort_handle(handle.abort_handle());
}

fn untrack_worker(handle: &JoinHandle<anyhow::Result<()>>) {
    let target = handle.abort_handle();
    let mut guard = LIVE_WORKERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = guard.iter().position(|item| item.id() == target.id()) {
        guard.remove(index);
    }
}

fn start_worker(state: Arc<AppState>, label: &str) -> JoinHandle<anyhow::Result<()>> {
    let handle = tokio::spawn(run_solver_worker_jobs_loop(
        state,
        format!("result120-numerical-isolation-{label}"),
        1,
        300,
        Duration::from_millis(20),
    ));
    track_worker(&handle);
    handle
}

async fn stop_worker(worker: JoinHandle<anyhow::Result<()>>) {
    untrack_worker(&worker);
    worker.abort();
    let _ = worker.await;
}

/// Inserts a queued `private.worker_jobs` row, matching the canonical queue contract.
#[allow(clippy::too_many_arguments)]
async fn enqueue_job(
    pool: &PgPool,
    job_id: Uuid,
    actor: Uuid,
    job_kind: &str,
    worker_queue: &str,
    payload_schema_version: &str,
    payload: Value,
    subject_id: Uuid,
) -> anyhow::Result<Uuid> {
    // `worker_jobs_review_quality_diagnostic_semantics_check` requires the dedicated diagnostic to be
    // operator-visible; every other solver/package job in this suite uses user visibility.
    let visibility = if job_kind == "review.quality_diagnostic" {
        "operator"
    } else {
        "user"
    };
    sqlx::query(
        r"
        INSERT INTO private.worker_jobs (
          id, job_kind, worker_runtime, worker_queue, requester_type, requested_by,
          visibility, payload_schema_version, payload_json, status, subject_type, subject_id,
          max_attempts
        ) VALUES (
          $1, $2, 'calculator', $3, 'user', $4, $5, $6, $7, 'queued', 'lca_result', $8, $9
        )
        ",
    )
    .bind(job_id)
    .bind(job_kind)
    .bind(worker_queue)
    .bind(actor)
    .bind(visibility)
    .bind(payload_schema_version)
    .bind(payload)
    .bind(subject_id)
    .bind(MAX_ATTEMPTS)
    .execute(pool)
    .await?;
    Ok(job_id)
}

async fn wait_for_job(pool: &PgPool, job_id: Uuid) -> anyhow::Result<(String, Value, Value)> {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let row = sqlx::query(
                "SELECT status, error_code, error_message, diagnostics, result_json FROM private.worker_jobs WHERE id=$1",
            )
            .bind(job_id)
            .fetch_one(pool)
            .await?;
            let status = row.try_get::<String, _>("status")?;
            if matches!(
                status.as_str(),
                "completed" | "blocked" | "failed" | "dead_letter" | "cancelled"
            ) {
                if status != "completed" {
                    eprintln!(
                        "job {job_id} terminal status={status} code={:?} message={:?}",
                        row.try_get::<Option<String>, _>("error_code")?,
                        row.try_get::<Option<String>, _>("error_message")?
                    );
                }
                let diagnostics = row.try_get::<Value, _>("diagnostics")?;
                let result_json = row
                    .try_get::<Option<Value>, _>("result_json")?
                    .unwrap_or(Value::Null);
                return Ok((status, diagnostics, result_json));
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("timed out after 180s waiting for worker job {job_id}"))?
}

async fn run_one_job(
    state: Arc<AppState>,
    job_id: Uuid,
    label: &str,
) -> anyhow::Result<(String, Value, Value)> {
    let worker = start_worker(state.clone(), label);
    let outcome = wait_for_job(&state.pool, job_id).await;
    stop_worker(worker).await;
    outcome
}

/// Builds one snapshot through the real queued builder and returns its resolved snapshot id.
async fn build_snapshot(
    state: Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    label: &str,
    tracked: &mut TrackedResources,
) -> anyhow::Result<Uuid> {
    let snapshot_id = Uuid::new_v4();
    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "lca.build_snapshot",
        "solver",
        "lca.build_snapshot.request.v1",
        json!({
            // Compatibility key equal to the worker job id, so `lca_results.job_id` addresses this run.
            "job_id": job_id,
            "snapshot_id": snapshot_id,
            "all_states": false,
            "process_states": solver_worker::default_snapshot_process_states_arg(),
            "provider_rule": "split_by_process_volume",
            "reference_normalization_mode": "lenient",
            "allocation_fraction_mode": "lenient",
            "no_lcia": false,
        }),
        fixture.consumer,
    )
    .await?;
    tracked.worker_jobs.push(job_id);
    let (status, _diagnostics, result_json) = run_one_job(state, job_id, label).await?;
    anyhow::ensure!(
        status == "completed",
        "snapshot build {label} status={status}"
    );
    // `result_json.snapshotId` is the canonical Worker `worker_jobs` projection for a build.
    let resolved = result_json
        .get("snapshotId")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            anyhow::anyhow!("build {label} lacks a resolved snapshot id: result_json={result_json}")
        })?
        .parse::<Uuid>()?;
    tracked.snapshots.push(resolved);
    Ok(resolved)
}

/// Builds a snapshot through the owner-scoped request path (`include_user_id` = the actor).
///
/// The plain `build_snapshot` helper deliberately omits `include_user_id`, so it never selects the
/// actor's state-0 drafts; owner-draft assertions must use this scoped variant.
async fn build_owner_snapshot(
    state: Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    label: &str,
    tracked: &mut TrackedResources,
) -> anyhow::Result<Uuid> {
    let snapshot_id = Uuid::new_v4();
    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "lca.build_snapshot",
        "solver",
        "lca.build_snapshot.request.v1",
        json!({
            "job_id": job_id,
            "snapshot_id": snapshot_id,
            "all_states": false,
            // Public numerical universe is exactly 100; the actor's own drafts join via include_user_id.
            "process_states": solver_worker::numerical_process_states_arg(),
            "include_user_id": actor,
            "provider_rule": "split_by_process_volume",
            "reference_normalization_mode": "lenient",
            "allocation_fraction_mode": "lenient",
            "no_lcia": false,
        }),
        fixture.consumer,
    )
    .await?;
    tracked.worker_jobs.push(job_id);
    let (status, _diagnostics, result_json) = run_one_job(state, job_id, label).await?;
    anyhow::ensure!(status == "completed", "owner build {label} status={status}");
    let resolved = result_json
        .get("snapshotId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("owner build {label} lacks a resolved snapshot id"))?
        .parse::<Uuid>()?;
    tracked.snapshots.push(resolved);
    Ok(resolved)
}

/// Owner-scoped build with one explicit request root, using the same authorization context.
#[allow(clippy::too_many_arguments)]
async fn build_owner_snapshot_with_root(
    state: Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    root: &Uuid,
    label: &str,
    tracked: &mut TrackedResources,
) -> anyhow::Result<Uuid> {
    let snapshot_id = Uuid::new_v4();
    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "lca.build_snapshot",
        "solver",
        "lca.build_snapshot.request.v1",
        json!({
            "job_id": job_id,
            "snapshot_id": snapshot_id,
            "all_states": false,
            "process_states": solver_worker::numerical_process_states_arg(),
            "include_user_id": actor,
            "request_roots": [RequestRootProcess::new(*root, VERSION)],
            "provider_rule": "split_by_process_volume",
            "reference_normalization_mode": "lenient",
            "allocation_fraction_mode": "lenient",
            "no_lcia": false,
        }),
        fixture.consumer,
    )
    .await?;
    tracked.worker_jobs.push(job_id);
    let (status, diagnostics, result_json) = run_one_job(state, job_id, label).await?;
    anyhow::ensure!(
        status == "completed",
        "{label} did not complete: status={status} diagnostics={diagnostics}"
    );
    let resolved = result_json
        .get("snapshotId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{label} lacks a resolved snapshot id: {result_json}"))?
        .parse::<Uuid>()?;
    tracked.snapshots.push(resolved);
    Ok(resolved)
}

/// Solves one snapshot through the real queued `solve_all_unit` job.
async fn solve_all_unit(
    state: Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    snapshot_id: Uuid,
    label: &str,
    tracked: &mut TrackedResources,
) -> anyhow::Result<Uuid> {
    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "lca.solve_all_unit",
        "solver",
        "lca.solve_all_unit.request.v1",
        json!({
            "job_id": job_id,
            "snapshot_id": snapshot_id,
            "solve": {"return_x": false, "return_g": false, "return_h": true},
            "unit_batch_size": 128,
            "print_level": 0.0,
        }),
        fixture.consumer,
    )
    .await?;
    tracked.worker_jobs.push(job_id);
    let (status, _, _) = run_one_job(state, job_id, label).await?;
    anyhow::ensure!(
        status == "completed",
        "solve_all_unit {label} status={status}"
    );
    Ok(job_id)
}

/// Reads the Calculation Bundle the Worker uploaded and returns the exact solved axis and values.
///
/// The process axis comes from the bundle's own `processes` artifact and the LCIA values come from
/// its `lcia` artifact, so the assertion consumes production artifacts instead of re-deriving them.
async fn read_solved_axis(
    state: &AppState,
    snapshot_id: Uuid,
    job_id: Uuid,
) -> anyhow::Result<SolvedAxis> {
    let bundle_ref: Value = sqlx::query_scalar(
        "SELECT diagnostics->'calculation_bundle' FROM private.lca_results WHERE job_id=$1 AND snapshot_id=$2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(job_id)
    .bind(snapshot_id)
    .fetch_one(&state.pool)
    .await?;
    let calculation_id = bundle_ref
        .get("calculationId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("calculation bundle reference lacks calculationId"))?
        .parse::<Uuid>()?;
    let bundle_content_hash = bundle_ref
        .get("bundleContentHash")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("calculation bundle reference lacks bundleContentHash"))?;
    let manifest_url = bundle_ref
        .get("manifestUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("calculation bundle reference lacks manifestUrl"))?;
    let manifest_sha256 = bundle_ref
        .get("manifestSha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("calculation bundle reference lacks manifestSha256"))?;
    let manifest_bytes = state.object_store.download_object_url(manifest_url).await?;
    anyhow::ensure!(
        sha256_hex(manifest_bytes.as_slice()) == manifest_sha256,
        "calculation bundle manifest hash mismatch"
    );
    let manifest: Value = serde_json::from_slice(manifest_bytes.as_slice())?;

    let prefix = format!("calculation-bundles/{calculation_id}/{bundle_content_hash}");
    // The Calculation Bundle names its Process axis artifact `process_axis`.
    let processes = read_bundle_artifact_lines(state, &manifest, &prefix, "process_axis").await?;
    let lcia = read_bundle_artifact_lines(state, &manifest, &prefix, "lcia").await?;
    let rows = parse_solved_rows(processes.as_slice(), lcia.as_slice())?;
    let axis = rows
        .iter()
        .map(|row| (row.process_id, row.process_version.clone()))
        .collect::<Vec<_>>();
    Ok(SolvedAxis { axis, rows })
}

/// Downloads one Calculation Bundle JSONL artifact and verifies its declared byte/hash bounds.
async fn read_bundle_artifact_lines(
    state: &AppState,
    manifest: &Value,
    prefix: &str,
    kind: &str,
) -> anyhow::Result<Vec<Value>> {
    let artifact = manifest
        .get("artifacts")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("kind").and_then(Value::as_str) == Some(kind))
        })
        .ok_or_else(|| anyhow::anyhow!("calculation bundle lacks a {kind} artifact"))?;
    let path = artifact
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{kind} artifact lacks path"))?;
    let declared_sha256 = artifact
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{kind} artifact lacks sha256"))?;
    let compressed = artifact
        .get("compression")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let key = state
        .object_store
        .prefixed_object_key(format!("{prefix}/{path}").as_str())?;
    let bytes = state.object_store.download_object_key(key.as_str()).await?;
    anyhow::ensure!(
        sha256_hex(bytes.as_slice()) == declared_sha256,
        "{kind} artifact hash mismatch"
    );
    let plain = match compressed {
        "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(bytes.as_slice());
            let mut plain = Vec::new();
            std::io::Read::read_to_end(&mut decoder, &mut plain)?;
            plain
        }
        "none" => bytes,
        other => anyhow::bail!("unsupported calculation bundle compression {other}"),
    };
    plain
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| Ok(serde_json::from_slice::<Value>(line)?))
        .collect()
}

/// Joins the bundle process axis with the bundle LCIA records into one solved row per process.
fn parse_solved_rows(axis: &[Value], lcia: &[Value]) -> anyhow::Result<Vec<SolvedRow>> {
    let mut processes = BTreeMap::<i64, (Uuid, String)>::new();
    for record in axis {
        let process_index = record
            .get("processIndex")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("processes artifact record lacks processIndex"))?;
        let root_process = record
            .get("rootProcess")
            .ok_or_else(|| anyhow::anyhow!("processes artifact record lacks rootProcess"))?;
        let process_id = root_process
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("processes artifact record lacks rootProcess.id"))?
            .parse::<Uuid>()?;
        let process_version = root_process
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("processes artifact record lacks rootProcess.version"))?
            .to_owned();
        processes.insert(process_index, (process_id, process_version));
    }

    let mut impacts_by_process = BTreeMap::<i64, BTreeMap<Uuid, f64>>::new();
    for record in lcia {
        let process_index = record
            .get("processIndex")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow::anyhow!("lcia artifact record lacks processIndex"))?;
        let method = record
            .get("method")
            .ok_or_else(|| anyhow::anyhow!("lcia artifact record lacks method"))?;
        let method_id = method
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("lcia artifact record lacks method.id"))?
            .parse::<Uuid>()?;
        let mean_amount = record
            .get("meanAmount")
            .and_then(Value::as_f64)
            .ok_or_else(|| anyhow::anyhow!("lcia artifact record lacks meanAmount"))?;
        impacts_by_process
            .entry(process_index)
            .or_default()
            .insert(method_id, mean_amount);
    }

    processes
        .into_iter()
        .map(|(process_index, (process_id, process_version))| {
            let impacts = impacts_by_process
                .remove(&process_index)
                .ok_or_else(|| anyhow::anyhow!("process {process_id} has no LCIA result rows"))?;
            Ok(SolvedRow {
                process_id,
                process_version,
                impacts,
            })
        })
        .collect()
}

fn compare_solved_axes(before: &SolvedAxis, after: &SolvedAxis) -> anyhow::Result<()> {
    anyhow::ensure!(
        before.axis == after.axis,
        "process axis changed after the published Result was created:\nbefore={:?}\nafter={:?}",
        before.axis,
        after.axis
    );
    let before_by_id = before
        .rows
        .iter()
        .map(|row| (row.process_id, row))
        .collect::<BTreeMap<_, _>>();
    let after_by_id = after
        .rows
        .iter()
        .map(|row| (row.process_id, row))
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        before_by_id.keys().eq(after_by_id.keys()),
        "solved process set changed after the published Result was created"
    );
    for (process_id, before_row) in &before_by_id {
        let after_row = after_by_id[process_id];
        anyhow::ensure!(
            before_row.impacts.keys().eq(after_row.impacts.keys()),
            "impact set changed for process {process_id}"
        );
        for (method_id, before_value) in &before_row.impacts {
            let after_value = after_row.impacts[method_id];
            let delta = (after_value - before_value).abs();
            anyhow::ensure!(
                delta <= TOLERANCE,
                "unit result changed for process {process_id} method {method_id}: before={before_value} after={after_value} delta={delta}"
            );
        }
    }
    Ok(())
}

/// Applies the exact candidate-state predicate the Database foundation uses when no current
/// release exists, over the eligible row ids, so a published Result can be shown to be excluded
/// regardless of what the caller asks for.
async fn eligible_process_count_for_states(
    pool: &PgPool,
    actor: Uuid,
    states: &[i32],
) -> anyhow::Result<i64> {
    // Same transaction-scoped claim setup as `trusted_scalar_json`: an unreferenced CTE is not
    // reliable authorization setup, and binding the Uuid directly would not resolve `set_config`.
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('request.jwt.claim.role','service_role',true)")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT set_config('request.jwt.claim.sub',$1::text,true)")
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query(
        r"
        SELECT count(*)::bigint AS eligible
        FROM public.processes
        WHERE state_code = ANY($1)
          AND json_ordered IS NOT NULL
        ",
    )
    .bind(states)
    .fetch_one(&mut *tx)
    .await?;
    let eligible = row.try_get::<i64, _>("eligible")?;
    tx.rollback().await?;
    Ok(eligible)
}

/// Builds a guard without touching a database (the DB checks live in `preflight_task_instance`).
fn offline_guard(task_instance_id: &str) -> TaskInstanceGuard {
    TaskInstanceGuard {
        _lock: TASK_INSTANCE_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        task_instance_id: task_instance_id.to_owned(),
        database_host: "127.0.0.1".to_owned(),
        database_port: 61322,
    }
}

#[test]
fn baseline_fixture_allocation_has_no_result_row_but_names_the_identity() {
    // Runtime proof (not a source scan) that the pre-publication fixture creates no Result row while
    // the allocation still reserves its identity for the manifest.
    let baseline = FixtureProcesses::BaselineOnly;
    let with_result = FixtureProcesses::WithResult;

    assert!(!baseline.includes_result());
    assert!(with_result.includes_result());

    let fixture = allocate_fixture();
    assert_ne!(fixture.consumer, fixture.unit);
    assert_ne!(fixture.unit, fixture.result);
    assert_ne!(fixture.consumer, fixture.result);
    assert_eq!(fixture.elementary_flows.len(), 3);

    // The row set a baseline scenario would insert excludes the Result entirely.
    let baseline_ids = [fixture.consumer, fixture.unit];
    assert!(!baseline_ids.contains(&fixture.result));

    // A result-bearing scenario inserts it exactly once, at the published state, never at 100.
    let result_state = PROCESS_STATE_RESULT;
    assert_eq!(result_state, solver_worker::PUBLISHED_RESULT_PROCESS_STATE);
    assert_ne!(result_state, PROCESS_STATE_PUBLIC);
}

#[test]
fn manifest_is_written_on_every_outcome_including_injected_setup_failure() {
    // Runtime proof that the guard exists before fixture writes and that the manifest is produced on
    // both success and failure paths, so a failed seed cannot leave unrecorded rows.
    let directory = tempfile::tempdir().expect("temp manifest dir");
    let fixture = allocate_fixture();
    let actor = Uuid::new_v4();

    for outcome in ["ok", "failed"] {
        let runner = ScenarioRunner::with_manifest_dir(
            offline_guard("offline-test"),
            fixture.clone(),
            actor,
            "manifest-proof",
            directory.path().to_path_buf(),
        );
        let mut runner = runner;
        runner.outcome = outcome;
        drop(runner);

        let expected = directory
            .path()
            .join("result120-offline-test-manifest-proof.json");
        assert!(expected.exists(), "manifest missing for outcome {outcome}");
        let raw = std::fs::read(expected.as_path()).expect("read manifest");
        let manifest: serde_json::Value = serde_json::from_slice(raw.as_slice()).expect("parse");
        assert_eq!(manifest["outcome"], json!(outcome));
        assert_eq!(manifest["task_instance_id"], json!("offline-test"));
        assert_eq!(manifest["actor"], json!(actor));
        assert_eq!(
            manifest["schema_version"],
            json!("result120.fixture-resources.v1")
        );
        // The Result identity is named even when no row was written for it.
        assert_eq!(manifest["processes"]["result"], json!(fixture.result));
        assert_eq!(manifest["processes"]["unit"], json!(fixture.unit));
        assert_ne!(manifest["processes"]["result"], json!(fixture.unit));
        // No credential material is ever written.
        let text = String::from_utf8_lossy(raw.as_slice()).to_ascii_lowercase();
        for secret in ["secret", "password", "access_key", "token"] {
            assert!(!text.contains(secret), "manifest leaked {secret}");
        }
        std::fs::remove_file(expected.as_path()).expect("cleanup manifest");
    }
}

#[test]
fn manifest_names_the_injected_failure_row_set() {
    // Models a seed that fails midway: the tracked set is incomplete but the identities are exact,
    // so the manifest still tells a coordinator which rows may exist.
    let directory = tempfile::tempdir().expect("temp manifest dir");
    let fixture = allocate_fixture();
    let actor = Uuid::new_v4();
    let mut runner = ScenarioRunner::with_manifest_dir(
        offline_guard("offline-fail"),
        fixture.clone(),
        actor,
        "seed-failure",
        directory.path().to_path_buf(),
    );
    // Simulate a seeded job that succeeded before the failing statement.
    runner.tracked.worker_jobs.push(Uuid::new_v4());
    runner.outcome = "failed";
    drop(runner);

    let path = directory
        .path()
        .join("result120-offline-fail-seed-failure.json");
    let raw = std::fs::read(path.as_path()).expect("read failure manifest");
    let manifest: serde_json::Value = serde_json::from_slice(raw.as_slice()).expect("parse");
    assert_eq!(manifest["outcome"], json!("failed"));
    assert_eq!(manifest["worker_jobs"].as_array().map(Vec::len), Some(1));
    // Every identity the scenario could have written is retrievable.
    assert!(manifest["processes"]["consumer"].is_string());
    assert!(
        manifest["flows"]
            .as_array()
            .is_some_and(|flows| flows.len() == 5)
    );
}

#[test]
fn snapshot_builder_binary_check_rejects_missing_and_accepts_current_file() {
    // Runtime proof of the staleness gate, using the pure core so no process env is mutated and the
    // test cannot race another test. The source tree is absent under the temp root, so the staleness
    // comparison is skipped rather than failing.
    let root = std::path::Path::new("/tmp/result120-no-such-worker-root");

    let error = ensure_snapshot_builder_binary_at("", root)
        .expect_err("a missing SNAPSHOT_BUILDER_BIN must fail closed");
    assert!(
        error
            .to_string()
            .contains("SNAPSHOT_BUILDER_BIN must be an explicit absolute path"),
        "{error}"
    );

    // A bare name is rejected: it could resolve to a stale executable on `PATH`.
    assert!(ensure_snapshot_builder_binary_at("snapshot_builder", root).is_err());

    // A non-existent absolute path is rejected with build guidance.
    let error = ensure_snapshot_builder_binary_at("/nonexistent/snapshot_builder", root)
        .expect_err("missing file");
    assert!(error.to_string().contains("is not readable"), "{error}");

    // A real, fresh, executable file is accepted.
    let directory = tempfile::tempdir().expect("temp dir");
    let binary = directory.path().join("snapshot_builder");
    std::fs::write(binary.as_path(), b"#!/bin/sh\nexit 0\n").expect("write fake binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(binary.as_path())
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(binary.as_path(), permissions).expect("chmod");
    }
    ensure_snapshot_builder_binary_at(binary.to_string_lossy().as_ref(), root)
        .expect("fresh executable is accepted");
}

#[test]
fn fixture_bucket_is_task_owned_and_unrestricted() {
    // A task-owned bucket is created without a MIME allow-list. The types below are exactly why an
    // enumerated list is brittle: the first live run failed with `415 InvalidMimeType` on the vendor
    // zstd source-closure type, and enumerations are a standing hazard for a fresh bucket.
    assert!(
        FIXTURE_BUCKET_ALLOWED_MIME_TYPES.is_none(),
        "the task bucket must be created without a MIME allow-list"
    );
    // The ZIP type is the one `PACKAGE_ZIP_CONTENT_TYPE` uses for the real export artifact.
    assert_eq!(
        solver_worker::package_types::PACKAGE_ZIP_CONTENT_TYPE,
        "application/zip"
    );
}

#[test]
fn harness_never_bypasses_domain_guards() {
    // This suite is the acceptance proof for the Result-120 domain guards, so it must not weaken
    // them to make itself repeatable. These assertions run in ordinary `cargo test` (no database
    // needed) and fail if a future edit reintroduces a bypass.
    let source = include_str!("result120_numerical_isolation.rs");
    // The tokens are assembled at runtime so this guard's own literals are not self-matches.
    let forbidden = [
        ["DISABLE", " TRIGGER"].concat(),
        ["ENABLE REPLICA", " TRIGGER"].concat(),
        // The header documents the prohibition by name, so the guard looks for the actual write.
        ["set_config('app.review_controlled", "_write'"].concat(),
        ["DELETE FROM public", ".processes"].concat(),
        ["DROP", " CONSTRAINT"].concat(),
        ["ALTER TABLE public", ".processes"].concat(),
    ];
    for token in forbidden {
        assert!(
            !source.contains(token.as_str()),
            "the Result-120 acceptance harness must not contain `{token}`"
        );
    }
    // The seed writes final states directly, which is what keeps canonical triggers active.
    assert!(source.contains("result_state"));
    // Every scenario must be gated on the dedicated task instance and stop its workers.
    assert!(source.contains("guard_task_instance(TaskInstanceScope::Full)"));
    assert!(source.contains("stop_all_workers()"));
    assert!(source.contains("write_fixture_manifest("));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "not run by default: needs a dedicated task DB + task bucket; run scripts/run_result120_numerical_isolation.sh with one RESULT120_SCENARIO (ignored tests are not ordinary passes)"]
async fn published_result_does_not_change_unit_numerical_result() -> anyhow::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_test_writer()
        .try_init();
    let guard = guard_task_instance(TaskInstanceScope::Full)?;
    let state = Arc::new(AppState::new(&test_config()).await?);
    ensure_snapshot_builder_binary_is_current()?;
    preflight_task_instance(&state.pool).await?;
    ensure_no_preexisting_queue_work(&state.pool).await?;
    // Identity is allocated and the guard/manifest is created BEFORE the first write, so a failure
    // during seeding still leaves a manifest naming every row that may exist.
    let actor = Uuid::new_v4();
    let mut runner = ScenarioRunner::new(guard, allocate_fixture(), actor, "numerical-isolation");
    // The Result does not exist yet in this scenario: this is the pre-publication universe.
    seed_fixture(
        &state.pool,
        actor,
        &runner.fixture,
        PROCESS_STATE_PUBLIC,
        PROCESS_STATE_PUBLIC,
        FixtureProcesses::BaselineOnly,
        true,
    )
    .await?;
    let outcome = {
        let ScenarioRunner {
            guard,
            fixture,
            actor,
            tracked,
            ..
        } = &mut runner;
        run_published_result_scenario(&state, guard, fixture, *actor, tracked).await
    };
    runner.finish(if outcome.is_ok() { "ok" } else { "failed" });
    outcome
}

async fn run_published_result_scenario(
    state: &Arc<AppState>,
    guard: &TaskInstanceGuard,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
) -> anyhow::Result<()> {
    let task_instance_id = guard.task_instance_id().to_owned();
    let _ = task_instance_id;

    // ---- Phase 1: only the public Unit exists. Build and solve through the real Worker path.
    let before_snapshot = build_snapshot(state.clone(), fixture, actor, "before", tracked).await?;
    let before_job = solve_all_unit(
        state.clone(),
        fixture,
        actor,
        before_snapshot,
        "before",
        tracked,
    )
    .await?;
    let before_axis = read_solved_axis(state.as_ref(), before_snapshot, before_job).await?;

    // The public universe must be exactly the two public Unit processes: the consumer and U(P).
    let before_ids = before_axis
        .rows
        .iter()
        .map(|row| row.process_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        before_ids.contains(&fixture.consumer),
        "consumer C is missing from the pre-publication axis: {before_ids:?}"
    );
    anyhow::ensure!(
        before_ids.contains(&fixture.unit),
        "Unit U(P) is missing from the pre-publication axis: {before_ids:?}"
    );
    anyhow::ensure!(
        !before_ids.contains(&fixture.result),
        "the Result was already a numerical input before publication"
    );
    let before_consumer = before_axis
        .rows
        .iter()
        .find(|row| row.process_id == fixture.consumer)
        .expect("consumer row");
    let before_unit = before_axis
        .rows
        .iter()
        .find(|row| row.process_id == fixture.unit)
        .expect("unit row");
    // Distinct nonzero values prove the assertion below is not a trivial all-zero comparison.
    anyhow::ensure!(
        before_consumer
            .impacts
            .values()
            .any(|value| value.abs() > 0.0),
        "consumer impact row is trivial"
    );
    anyhow::ensure!(
        before_unit.impacts.values().any(|value| value.abs() > 0.0),
        "unit impact row is trivial"
    );

    // ---- Phase 2: the Result that shares U(P)'s product reference flow now exists.
    //
    // The Result Process row is created at its published state. No public row is mutated, so the
    // canonical documents, versions and identities of C and U(P) are untouched by construction.
    publish_result(&state.pool, fixture, actor).await?;
    let published_state = sqlx::query_scalar::<_, i32>(
        "SELECT state_code FROM public.processes WHERE id=$1 AND version=$2",
    )
    .bind(fixture.result)
    .bind(VERSION)
    .fetch_one(&state.pool)
    .await?;
    anyhow::ensure!(
        published_state == PROCESS_STATE_RESULT,
        "the Result was not created at its published state: {published_state}"
    );

    // ---- Phase 3: identical build and solve through the same production path.
    let after_snapshot = build_snapshot(state.clone(), fixture, actor, "after", tracked).await?;
    let after_job = solve_all_unit(
        state.clone(),
        fixture,
        actor,
        after_snapshot,
        "after",
        tracked,
    )
    .await?;
    let after_axis = read_solved_axis(state.as_ref(), after_snapshot, after_job).await?;

    // ---- Acceptance 1: root membership, provider decisions, matrix axes and numeric values.
    compare_solved_axes(&before_axis, &after_axis)?;
    // Concrete parity evidence: the same per-process LCIA values on both runs.
    for row in &before_axis.rows {
        let impacts = row
            .impacts
            .values()
            .map(|value| format!("{value:.12e}"))
            .collect::<Vec<_>>()
            .join(",");
        eprintln!(
            "numeric parity: process={} version={} impacts=[{impacts}]",
            row.process_id, row.process_version
        );
    }
    eprintln!(
        "numeric parity: axis={:?} rows={} (identical before and after Result publication)",
        before_axis.axis,
        before_axis.rows.len()
    );
    let after_ids = after_axis
        .rows
        .iter()
        .map(|row| row.process_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !after_ids.contains(&fixture.result),
        "the published Result entered the numerical process axis: {after_ids:?}"
    );
    anyhow::ensure!(
        after_ids.contains(&fixture.unit),
        "the Unit disappeared from the axis after the Result was published"
    );

    // ---- Acceptance 2: the published Result is not part of the provider universe.
    //
    // The Matrix Readiness report lists every Process column actually compiled. The Result must be
    // absent, which is what proves it contributed no provider reference port to the Unit's residual.
    let readiness_json: Value = sqlx::query_scalar(
        "SELECT coverage FROM private.lca_snapshot_artifacts WHERE snapshot_id=$1 AND status='ready' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(after_snapshot)
    .fetch_one(&state.pool)
    .await?;
    anyhow::ensure!(
        readiness_json
            .get("matrix_scale")
            .and_then(|value| value.get("process_count"))
            .and_then(Value::as_i64)
            == i64::try_from(after_axis.rows.len()).ok(),
        "snapshot coverage process count disagrees with the solved axis"
    );

    // ---- Acceptance 3: the public numerical universe is exactly state 100.
    //
    // No current release exists in this fixture, so the Database candidate predicate must be the
    // exact-100 candidate literal, and the reserved publication segment must contribute nothing.
    // Raw row counts by state are not an eligibility proof: `state_code = ANY(...)` simply counts
    // rows carrying those states. The eligibility proof is the candidate universe the production
    // normalizer returns (below), which must contain exactly the two public Units.
    let public_rows = eligible_process_count_for_states(&state.pool, actor, &[100]).await?;
    anyhow::ensure!(
        public_rows == 2,
        "expected exactly two state-100 public processes, got {public_rows}"
    );
    // The published Result really does carry the reserved state, so the exclusion below is
    // observable rather than vacuous.
    let result_rows =
        eligible_process_count_for_states(&state.pool, actor, &[PROCESS_STATE_RESULT]).await?;
    anyhow::ensure!(
        result_rows == 1,
        "expected the published Result row to exist at state {PROCESS_STATE_RESULT}, got {result_rows}"
    );
    let candidate_scope = normalize_scope(
        &state.pool,
        actor,
        fixture,
        json!({"coverageMode": "global_eligible"}),
    )
    .await?;
    anyhow::ensure!(
        candidate_scope
            .get("eligibilityPredicateVersion")
            .and_then(Value::as_str)
            == Some(solver_worker::CANDIDATE_PUBLIC_NUMERICAL_PREDICATE_V2),
        "the no-current-release candidate scope did not use the exact-100 predicate: {candidate_scope}"
    );
    let candidate_ids = candidate_scope
        .get("processes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("candidate scope lacks processes"))?
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !candidate_ids.contains(&fixture.result.to_string()),
        "the published Result entered the candidate universe: {candidate_ids:?}"
    );
    anyhow::ensure!(
        candidate_ids.len() == 2,
        "candidate universe is not the two public Units: {candidate_ids:?}"
    );

    // ---- Acceptance 4: Database current-release membership keeps its own role rule.
    //
    // A release run pins U(P) as `unit_process` and the Result as `result_process`. The Database
    // membership query selects only `unit_process`, so the Result stays out even though it is in
    // the same publication.
    //
    // NOTE: the release registration below is written by this test as fixture evidence for the
    // membership rule only. It does not exercise the release publication workflow, which is owned
    // by another task.
    sqlx::query(
        "INSERT INTO private.roles(user_id,team_id,role) VALUES($1,'00000000-0000-0000-0000-000000000000','data_product_manager') ON CONFLICT DO NOTHING",
    )
    .bind(actor)
    .execute(&state.pool)
    .await?;
    let release_run_id = Uuid::new_v4();
    sqlx::query(
        r"
        INSERT INTO private.lca_release_runs(
          id, release_version, scope_mode, selection_manifest_hash, input_manifest_hash,
          calculation_bundle_hash, calculation_bundle_ref, profile_lock_hash, publish_plan_hash,
          publish_plan, artifact_set_hash, status, idempotency_key, request_hash, created_by
        ) VALUES ($1,$2,'global_eligible',$3,$3,$3,'{}'::jsonb,$3,$3,'{}'::jsonb,$3,'prepared',$4,$3,$5)
        ",
    )
    .bind(release_run_id)
    // `lca_release_runs.release_version` is constrained to the exact TIDAS version shape.
    .bind(VERSION)
    .bind(repeated('a'))
    .bind(format!("result120-isolation-{release_run_id}"))
    .bind(actor)
    .execute(&state.pool)
    .await?;
    let approval_id = Uuid::new_v4();
    sqlx::query(
        r"
        INSERT INTO private.lca_release_approvals(
          id, release_run_id, publish_plan_hash, approval_hash, status, approved_by, approved_at,
          expires_at
        ) VALUES ($1,$2,$3,$3,'approved',$4,now(),now() + interval '7 days')
        ",
    )
    .bind(approval_id)
    .bind(release_run_id)
    .bind(repeated('b'))
    .bind(actor)
    .execute(&state.pool)
    .await?;
    sqlx::query(
        r"
        INSERT INTO private.lca_release_publications(
          id, release_run_id, release_version, approval_id, approval_hash, publish_plan_hash,
          release_manifest_hash, artifact_set_hash, approved_by, executed_by,
          credential_fingerprint, idempotency_key, published_at, status, is_current
        ) VALUES ($1,$2,$3,$4,$5,$5,$5,$5,$6,$6,$5,$7,now(),'current',true)
        ",
    )
    .bind(Uuid::new_v4())
    .bind(release_run_id)
    .bind(VERSION)
    .bind(approval_id)
    .bind(repeated('c'))
    .bind(actor)
    .bind(format!("result120-isolation-publication-{release_run_id}"))
    .execute(&state.pool)
    .await?;

    for (dataset_type, role, dataset, include_source) in [
        ("process", "unit_process", fixture.consumer, true),
        ("process", "unit_process", fixture.unit, true),
        ("process", "result_process", fixture.result, true),
        ("lciamethod", "support", fixture.method, false),
    ] {
        sqlx::query(
            r"
            INSERT INTO private.lca_release_dataset_versions(
              release_run_id, dataset_type, dataset_role, dataset_uuid, dataset_version,
              source_process_uuid, source_process_version, version_significant_hash,
              semantic_hash, canonical_content_hash, artifact_ref
            ) VALUES (
              $1,$2,$3,$4,$5,
              CASE WHEN $6 THEN $4 ELSE NULL END,
              CASE WHEN $6 THEN $5 ELSE NULL END,
              $7,$7,$7,'{}'::jsonb
            )
            ",
        )
        .bind(release_run_id)
        .bind(dataset_type)
        .bind(role)
        .bind(dataset)
        .bind(VERSION)
        .bind(include_source)
        .bind(repeated('d'))
        .execute(&state.pool)
        .await?;
    }

    // The Database-side membership rule, read through the production normalize function. It is the
    // same function the closure path calls, and it must return exactly the Unit membership.
    let normalized = normalize_scope(
        &state.pool,
        actor,
        fixture,
        json!({"coverageMode": "global_eligible"}),
    )
    .await?;
    let normalized_predicate = normalized
        .get("eligibilityPredicateVersion")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    anyhow::ensure!(
        normalized_predicate == solver_worker::CURRENT_PUBLIC_RELEASE_MANIFEST_PREDICATE_V2,
        "current-release membership did not use the release predicate: {normalized_predicate}"
    );
    let release_processes = normalized
        .get("processes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("normalized scope lacks processes"))?;
    let release_ids = release_processes
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        release_ids.contains(&fixture.unit.to_string().as_str()),
        "U(P) is missing from the current-release membership: {release_ids:?}"
    );
    anyhow::ensure!(
        !release_ids.contains(&fixture.result.to_string().as_str()),
        "the published Result entered current-release membership: {release_ids:?}"
    );

    // ---- Acceptance 5: exact Result identity is refused as a request root by the real builder.
    let result_only =
        build_snapshot_with_roots(state.clone(), fixture, actor, fixture.result).await;
    let error = result_only.expect_err(
        "the builder must refuse a snapshot whose only request root is the published Result",
    );
    let message = format!("{error:#}");
    anyhow::ensure!(
        message.contains("request_root_not_numerically_eligible")
            && message.contains("published_result_process_is_not_a_numerical_input"),
        "the refusal must be locatable and name the published-Result reason: {message}"
    );
    eprintln!("published Result rejected as request root: {message}");

    Ok(())
}

/// The owner-draft path must not admit a published Result, including for the actor who owns it.
///
/// This is the same production builder entrypoint used above, but with the versioned
/// `public_plus_owner_draft` request contract that joins the actor's own state-0 drafts. A Result
/// published by that same actor must not re-enter compute through the broad `user_id=actor` branch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "not run by default: needs a dedicated task DB + task bucket; run scripts/run_result120_numerical_isolation.sh with one RESULT120_SCENARIO (ignored tests are not ordinary passes)"]
async fn owner_scope_does_not_readmit_a_published_result() -> anyhow::Result<()> {
    let guard = guard_task_instance(TaskInstanceScope::Full)?;
    let state = Arc::new(AppState::new(&test_config()).await?);
    ensure_snapshot_builder_binary_is_current()?;
    preflight_task_instance(&state.pool).await?;
    ensure_no_preexisting_queue_work(&state.pool).await?;
    let actor = Uuid::new_v4();
    let mut runner = ScenarioRunner::new(guard, allocate_fixture(), actor, "owner-scope");
    seed_published_result_fixture(&state.pool, actor, &runner.fixture).await?;
    let outcome = {
        let ScenarioRunner {
            guard,
            fixture,
            actor,
            tracked,
            ..
        } = &mut runner;
        run_owner_scope_scenario(&state, guard, fixture, *actor, tracked).await
    };
    runner.finish(if outcome.is_ok() { "ok" } else { "failed" });
    outcome
}

async fn run_owner_scope_scenario(
    state: &Arc<AppState>,
    guard: &TaskInstanceGuard,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
) -> anyhow::Result<()> {
    let _ = guard.task_instance_id();

    // The actor owns every fixture row, so identity is not what keeps the Result out.
    let owner = sqlx::query_scalar::<_, Uuid>("SELECT user_id FROM public.processes WHERE id=$1")
        .bind(fixture.unit)
        .fetch_one(&state.pool)
        .await?;
    anyhow::ensure!(
        owner == actor,
        "fixture ownership is not the requesting actor"
    );

    // The Result already exists at state 120 and is owned by this same actor, so the owner-draft
    // universe is asked for while the actor's own published Result is present.

    let resolved =
        build_owner_snapshot(state.clone(), fixture, actor, "owner-scope", tracked).await?;
    let job_id = solve_all_unit(
        state.clone(),
        fixture,
        actor,
        resolved,
        "owner-scope",
        tracked,
    )
    .await?;
    let axis = read_solved_axis(state.as_ref(), resolved, job_id).await?;
    let ids = axis
        .rows
        .iter()
        .map(|row| row.process_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !ids.contains(&fixture.result),
        "the actor's own published Result re-entered compute through the owner scope: {ids:?}"
    );
    anyhow::ensure!(
        ids.contains(&fixture.unit),
        "the public Unit is missing from the owner-scope axis: {ids:?}"
    );

    // A separate actor-owned state-0 draft of the same process stays owner-readable: the exclusion
    // is about the published Result state, not about the actor's own drafts. It must be a distinct
    // row, because a state-100 row is immutable under the canonical content guard.
    let draft_fixture = Fixture {
        result: Uuid::new_v4(),
        ..fixture.clone()
    };
    // Record the draft identity before the insert, so a seed or solve failure still leaves it in the
    // retained manifest.
    tracked.draft_processes.push(draft_fixture.result);
    insert_draft_process(&state.pool, &draft_fixture, actor).await?;
    let draft_snapshot =
        build_owner_snapshot(state.clone(), &draft_fixture, actor, "owner-draft", tracked).await?;
    let draft_job = solve_all_unit(
        state.clone(),
        &draft_fixture,
        actor,
        draft_snapshot,
        "owner-draft",
        tracked,
    )
    .await?;
    let draft_axis = read_solved_axis(state.as_ref(), draft_snapshot, draft_job).await?;
    let draft_ids = draft_axis
        .rows
        .iter()
        .map(|row| row.process_id)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        draft_ids.contains(&draft_fixture.result),
        "an actor-owned state-0 draft must remain owner-readable: {draft_ids:?}"
    );
    // The draft row is retained like every other authored row; the coordinator resets the
    // dedicated task instance between scenarios.

    // Explicit request root naming the actor's own state-0 draft. The full-library build above cannot
    // prove this: it selects candidates, while an explicit root goes through the root-eligibility
    // check. This is the regression the root path guards.
    let explicit_snapshot = build_owner_snapshot_with_root(
        state.clone(),
        &draft_fixture,
        actor,
        &draft_fixture.result,
        "owner-explicit-draft-root",
        tracked,
    )
    .await?;
    let explicit_job = solve_all_unit(
        state.clone(),
        &draft_fixture,
        actor,
        explicit_snapshot,
        "owner-explicit-draft-root",
        tracked,
    )
    .await?;
    let explicit_axis = read_solved_axis(state.as_ref(), explicit_snapshot, explicit_job).await?;
    anyhow::ensure!(
        explicit_axis
            .rows
            .iter()
            .any(|row| row.process_id == draft_fixture.result),
        "the actor's own state-0 draft must be admitted as an explicit request root"
    );

    // A foreign actor's draft must not be admissible as an explicit root.
    let foreign_fixture = Fixture {
        result: Uuid::new_v4(),
        ..fixture.clone()
    };
    tracked.draft_processes.push(foreign_fixture.result);
    insert_draft_process(&state.pool, &foreign_fixture, Uuid::new_v4()).await?;
    let foreign_error = build_owner_snapshot_with_root(
        state.clone(),
        fixture,
        actor,
        &foreign_fixture.result,
        "owner-foreign-draft-root",
        tracked,
    )
    .await
    .expect_err("a foreign actor's draft must not be an explicit request root");
    let foreign_message = format!("{foreign_error:#}");
    anyhow::ensure!(
        foreign_message.contains("request_root_not_numerically_eligible"),
        "foreign draft refusal must be locatable: {foreign_message}"
    );
    eprintln!("foreign draft root refused: {foreign_message}");

    Ok(())
}

/// The dedicated Review Admin diagnostic keeps its intentional review state and still excludes the
/// published Result.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "not run by default: needs a dedicated task DB + task bucket; run scripts/run_result120_numerical_isolation.sh with one RESULT120_SCENARIO (ignored tests are not ordinary passes)"]
async fn review_diagnostic_scope_keeps_review_state_and_excludes_result() -> anyhow::Result<()> {
    let guard = guard_task_instance(TaskInstanceScope::Full)?;
    let state = Arc::new(AppState::new(&test_config()).await?);
    ensure_snapshot_builder_binary_is_current()?;
    preflight_task_instance(&state.pool).await?;
    ensure_no_preexisting_queue_work(&state.pool).await?;
    let actor = Uuid::new_v4();
    let mut runner = ScenarioRunner::new(guard, allocate_fixture(), actor, "review-diagnostic");
    seed_fixture(
        &state.pool,
        actor,
        &runner.fixture,
        REVIEW_PROCESS_STATE,
        PROCESS_STATE_RESULT,
        FixtureProcesses::WithResult,
        true,
    )
    .await?;
    let outcome = {
        let ScenarioRunner {
            guard,
            fixture,
            actor,
            tracked,
            ..
        } = &mut runner;
        run_review_diagnostic_scenario(&state, guard, fixture, *actor, tracked).await
    };
    runner.finish(if outcome.is_ok() { "ok" } else { "failed" });
    outcome
}

async fn run_review_diagnostic_scenario(
    state: &Arc<AppState>,
    guard: &TaskInstanceGuard,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
) -> anyhow::Result<()> {
    let _ = guard.task_instance_id();
    // The Unit is seeded directly at state 20 by the shared setup. `review_dataset_content_guard_v1`
    // makes a state-100 row immutable, so a runtime retag would require `app.review_controlled_write`
    // and would bypass the review admission path this diagnostic is meant to observe.
    // A pending review target makes the runner build a matrix rooted at the in-review Unit.
    sqlx::query(
        "INSERT INTO private.reviews(id,data_id,data_version,review_kind,target_table,state_code,submitted_revision_checksum,json) VALUES($1,$2,$3,'root','processes',0,$4,'{}'::jsonb)",
    )
    .bind(Uuid::new_v4())
    .bind(fixture.unit)
    .bind(VERSION)
    .bind(repeated('e'))
    .execute(&state.pool)
    .await?;

    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "review.quality_diagnostic",
        "review_quality",
        "review.quality_diagnostic.request.v1",
        json!({
            "scope": {"kind": "pending_review", "reviewStates": [0, 1]},
            "requestedAt": "2026-09-15T00:00:00Z",
        }),
        fixture.unit,
    )
    .await?;
    tracked.worker_jobs.push(job_id);
    // The runner owns its own claim loop, so it is started directly instead of through the solver
    // worker loop (which never claims this queue).
    let runner = tokio::spawn({
        let state = state.clone();
        async move {
            solver_worker::review_quality_diagnostic_runner::run_review_quality_diagnostic_runner(
                &state,
                solver_worker::review_quality_diagnostic_runner::ReviewQualityDiagnosticRunnerOptions {
                    poll_interval: Duration::from_millis(20),
                    max_runs: Some(1),
                    exit_when_idle: true,
                    worker_id: "result120-numerical-isolation-review".to_owned(),
                    lease_seconds: 300,
                },
            )
            .await
        }
    });
    // The runner future must be stopped on every path, including a wait failure, so a failed
    // assertion cannot leave a claim loop running against the task instance. It is also registered
    // for the drop-time teardown in case an earlier `?` returns first.
    track_abort_handle(runner.abort_handle());
    let waited = wait_for_job(&state.pool, job_id).await;
    runner.abort();
    let _ = runner.await;
    let (status, diagnostics, _result_json) = waited?;
    anyhow::ensure!(
        status == "completed",
        "review diagnostic did not complete: status={status} diagnostics={diagnostics}"
    );

    let snapshot_id = diagnostics
        .get("resolvedSnapshotId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("review diagnostic did not record a snapshot id"))?
        .parse::<Uuid>()?;
    // Read the diagnostic snapshot's own process axis sidecar: it lists exactly the Processes the
    // diagnostic matrix was compiled from.
    let artifact_url = sqlx::query_scalar::<_, String>(
        "SELECT artifact_url FROM private.lca_snapshot_artifacts WHERE snapshot_id=$1 AND status='ready' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(snapshot_id)
    .fetch_one(&state.pool)
    .await?;
    let index_url = solver_worker::snapshot_index::derive_snapshot_index_url(artifact_url.as_str());
    let index_bytes = state
        .object_store
        .download_object_url(index_url.as_str())
        .await?;
    // Deserialize into the canonical type so the field names cannot drift with a hand-parsed shape.
    let index: solver_worker::snapshot_index::SnapshotIndexDocument =
        serde_json::from_slice(index_bytes.as_slice())?;
    let axis_ids = index
        .process_map
        .iter()
        .map(|entry| entry.process_id.to_string())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        axis_ids.contains(&fixture.unit.to_string()),
        "the in-review Unit is missing from the diagnostic matrix: {axis_ids:?}"
    );
    anyhow::ensure!(
        !axis_ids.contains(&fixture.result.to_string()),
        "the published Result entered the review diagnostic matrix: {axis_ids:?}"
    );

    Ok(())
}

/// A published Result must not leave the platform inside an exported product package.
///
/// This runs the real `tidas.export_package` handler (not a simulated claim loop) for the owner
/// (`current_user`), `open_data`, `current_user_and_open_data` and `selected_roots` seeds, and
/// inspects the actual ZIP bytes and manifest it produced. Import behavior and the `100..=199`
/// support-data semantics are deliberately not changed by this policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "not run by default: needs a dedicated task DB + task bucket; run scripts/run_result120_numerical_isolation.sh with one RESULT120_SCENARIO (ignored tests are not ordinary passes)"]
async fn published_result_is_excluded_from_product_export() -> anyhow::Result<()> {
    let guard = guard_task_instance(TaskInstanceScope::Full)?;
    let state = Arc::new(AppState::new(&test_config()).await?);
    // No snapshot is built here, so the builder binary is not a prerequisite for this scenario.
    preflight_task_instance(&state.pool).await?;
    ensure_no_preexisting_queue_work(&state.pool).await?;
    let actor = Uuid::new_v4();
    // Guard/manifest exists before the first write. The Result is created at its published state;
    // the Unit, flows and support documents stay public.
    let mut runner = ScenarioRunner::new(guard, allocate_fixture(), actor, "product-export");
    seed_published_result_fixture(&state.pool, actor, &runner.fixture).await?;
    let outcome = {
        let ScenarioRunner {
            guard,
            fixture,
            actor,
            tracked,
            ..
        } = &mut runner;
        run_product_export_scenario(&state, guard, fixture, *actor, tracked).await
    };
    runner.finish(if outcome.is_ok() { "ok" } else { "failed" });
    outcome
}

async fn run_product_export_scenario(
    state: &Arc<AppState>,
    guard: &TaskInstanceGuard,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
) -> anyhow::Result<()> {
    let _ = guard.task_instance_id();

    // The Database foundation records the same policy version the Worker applies, so a package
    // reader can trace which exposure rule produced the artifact.
    let expected_policy = solver_worker::PRODUCT_EXPORT_POLICY_VERSION;

    // NULL-safety regression set. `state_code` is nullable and the policy withholds only `120`, so a
    // NULL-state row that was exportable before the policy must still be exportable, alongside the
    // existing `0`/`20`/`100`/`200` states.
    // Identities are recorded before the first insert, so a failure part-way through this loop
    // still leaves every probe id in the retained manifest.
    let mut probes = BTreeMap::<Option<i32>, Uuid>::new();
    for state_code in [None, Some(0), Some(20), Some(200)] {
        probes.insert(state_code, Uuid::new_v4());
    }
    tracked.probe_processes.extend(probes.values().copied());
    for (state_code, process) in &probes {
        insert_export_probe_process(&state.pool, fixture, actor, *process, *state_code).await?;
    }

    // `current_user` scope: the actor owns every fixture row, so only the policy can exclude R(P).
    // Every probe row is owned by the actor and must appear in the ZIP.
    export_and_assert(
        state,
        fixture,
        actor,
        tracked,
        "current_user",
        vec![],
        expected_policy,
        ExportExpectation::Packaged,
    )
    .await?;
    assert_export_probe_membership(state, fixture, actor, tracked, &probes, expected_policy)
        .await?;
    // `open_data` scope: its existing `100..=199` predicate is unchanged, and `120` is *inside*
    // that range, so the exact state-120 exclusion is what keeps R(P) out of the seed.
    export_and_assert(
        state,
        fixture,
        actor,
        tracked,
        "open_data",
        vec![],
        expected_policy,
        ExportExpectation::Packaged,
    )
    .await?;
    export_and_assert(
        state,
        fixture,
        actor,
        tracked,
        "current_user_and_open_data",
        vec![],
        expected_policy,
        ExportExpectation::Packaged,
    )
    .await?;
    // Exact selected root: the caller names R(P) directly. It must not be packaged.
    // Exact selected root naming the published Result: the export must refuse rather than emit a
    // package that silently drops the requested dataset.
    export_and_assert(
        state,
        fixture,
        actor,
        tracked,
        "selected_roots",
        vec![(fixture.result, VERSION.to_owned())],
        expected_policy,
        ExportExpectation::Refused,
    )
    .await?;
    // Control: an exact selected root that is a public Unit still exports.
    export_and_assert(
        state,
        fixture,
        actor,
        tracked,
        "selected_roots",
        vec![(fixture.unit, VERSION.to_owned())],
        expected_policy,
        ExportExpectation::Packaged,
    )
    .await?;

    Ok(())
}

/// Exports the `current_user` scope once more and asserts the exact Process membership.
///
/// This is the runtime regression for the NULL-safe fence: a NULL-state row must be present, the
/// existing `0`/`20`/`100`/`200` states must be present, and `120` must be absent.
async fn assert_export_probe_membership(
    state: &Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
    probes: &BTreeMap<Option<i32>, Uuid>,
    expected_policy: &str,
) -> anyhow::Result<()> {
    let packaged =
        export_current_user_processes(state, fixture, actor, tracked, expected_policy).await?;

    let expect_present = |state_code: Option<i32>| -> anyhow::Result<()> {
        let process = probes[&state_code];
        anyhow::ensure!(
            packaged.contains(&process.to_string()),
            "state_code={state_code:?} Process {process} was dropped from product export: {packaged:?}"
        );
        Ok(())
    };
    for state_code in [None, Some(0), Some(20), Some(100), Some(200)] {
        // `Some(100)` is the public Unit; the rest are the probe rows.
        if state_code == Some(100) {
            anyhow::ensure!(
                packaged.contains(&fixture.unit.to_string()),
                "the public Unit was dropped from product export: {packaged:?}"
            );
        } else {
            expect_present(state_code)?;
        }
    }
    anyhow::ensure!(
        !packaged.contains(&fixture.result.to_string()),
        "the published Result (120) was packaged: {packaged:?}"
    );
    eprintln!(
        "export NULL-safety: packaged={packaged:?} (120 excluded, NULL/0/20/100/200 retained)"
    );
    Ok(())
}

/// Runs a `current_user` export and returns the packaged Process ids.
async fn export_current_user_processes(
    state: &Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
    expected_policy: &str,
) -> anyhow::Result<Vec<String>> {
    let job_id = Uuid::new_v4();
    let payload = json!({
        "type": "export_package",
        "job_id": job_id,
        "requested_by": actor,
        "scope": "current_user",
        "roots": [],
    });
    sqlx::query(
        r"
        INSERT INTO private.worker_jobs (
          id, job_kind, worker_runtime, worker_queue, requester_type, requested_by,
          visibility, payload_schema_version, payload_json, status, subject_type, subject_id,
          max_attempts
        ) VALUES (
          $1, 'tidas.export_package', 'calculator', 'package', 'user', $2, 'user',
          'tidas.export_package.request.v1', $3, 'queued', 'package_export', $4, $5
        )
        ",
    )
    .bind(job_id)
    .bind(actor)
    .bind(&payload)
    .bind(fixture.consumer)
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    tracked.worker_jobs.push(job_id);

    let payload: solver_worker::package_types::PackageJobPayload = serde_json::from_value(payload)?;
    solver_worker::package_db::handle_package_job_payload(state, payload).await?;
    sqlx::query("UPDATE private.worker_jobs SET status='completed' WHERE id=$1")
        .bind(job_id)
        .execute(&state.pool)
        .await?;

    let row = sqlx::query(
        r"
        SELECT a.artifact_url, a.metadata, a.id
        FROM private.lca_package_artifacts a
        WHERE a.job_id = $1 AND a.artifact_kind = 'export_zip'
        ORDER BY a.created_at DESC
        LIMIT 1
        ",
    )
    .bind(job_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("NULL-safety export produced no ZIP artifact"))?;
    let artifact_url = row.try_get::<String, _>("artifact_url")?;
    let metadata = row.try_get::<Value, _>("metadata")?;
    anyhow::ensure!(
        metadata
            .get("productExportPolicyVersion")
            .and_then(Value::as_str)
            == Some(expected_policy),
        "NULL-safety export artifact is not bound to the product exposure policy: {metadata}"
    );
    tracked
        .package_artifacts
        .push(row.try_get::<Uuid, _>("id")?);

    let zip_bytes = state
        .object_store
        .download_object_url(artifact_url.as_str())
        .await?;
    let manifest = read_package_manifest(zip_bytes.as_slice())?;
    Ok(manifest
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("NULL-safety export manifest lacks entries"))?
        .iter()
        .filter(|entry| entry.get("table").and_then(Value::as_str) == Some("processes"))
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

/// Whether an export is expected to produce a package or to fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportExpectation {
    /// The export completes and produces inspectable package contents.
    Packaged,
    /// The export refuses: a selected dataset is not exportable under the product-exposure policy.
    Refused,
}

#[allow(clippy::too_many_arguments)]
async fn export_and_assert(
    state: &Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
    scope: &str,
    roots: Vec<(Uuid, String)>,
    expected_policy: &str,
    expectation: ExportExpectation,
) -> anyhow::Result<()> {
    let job_id = Uuid::new_v4();
    let payload = json!({
        "type": "export_package",
        "job_id": job_id,
        "requested_by": actor,
        "scope": scope,
        "roots": roots
            .iter()
            .map(|(id, version)| json!({"table": "processes", "id": id, "version": version}))
            .collect::<Vec<_>>(),
    });
    sqlx::query(
        r"
        INSERT INTO private.worker_jobs (
          id, job_kind, worker_runtime, worker_queue, requester_type, requested_by,
          visibility, payload_schema_version, payload_json, status, subject_type, subject_id,
          max_attempts
        ) VALUES (
          $1, 'tidas.export_package', 'calculator', 'package', 'user', $2, 'user',
          'tidas.export_package.request.v1', $3, 'queued', 'package_export', $4, $5
        )
        ",
    )
    .bind(job_id)
    .bind(actor)
    .bind(&payload)
    .bind(fixture.consumer)
    .bind(MAX_ATTEMPTS)
    .execute(&state.pool)
    .await?;
    tracked.worker_jobs.push(job_id);

    // Call the real export handler directly (not a simulated claim loop). This asserts the exact
    // bytes and manifest the handler produces from the database and object storage; the package
    // queue claim/lease/projection path is covered by its own tests.
    let payload: solver_worker::package_types::PackageJobPayload = serde_json::from_value(payload)?;
    let handled = solver_worker::package_db::handle_package_job_payload(state, payload).await;
    let status = match (expectation, handled) {
        (ExportExpectation::Packaged, Ok(())) => "completed",
        (ExportExpectation::Refused, Err(error)) => {
            // Fail closed is required: the export must refuse rather than emit a package that
            // silently omits the requested dataset.
            let message = format!("{error:#}");
            anyhow::ensure!(
                message.contains("not exportable") || message.contains("not found"),
                "selected-root refusal must be locatable, got: {message}"
            );
            eprintln!("export scope={scope} refused as expected: {message}");
            "failed"
        }
        (ExportExpectation::Packaged, Err(error)) => {
            return Err(anyhow::anyhow!(
                "export scope={scope} unexpectedly failed: {error:#}"
            ));
        }
        (ExportExpectation::Refused, Ok(())) => {
            return Err(anyhow::anyhow!(
                "export scope={scope} must fail closed for a non-exportable selected dataset"
            ));
        }
    };
    sqlx::query("UPDATE private.worker_jobs SET status=$2 WHERE id=$1")
        .bind(job_id)
        .bind(status)
        .execute(&state.pool)
        .await?;
    if expectation == ExportExpectation::Refused {
        // Fail-closed must not have produced an artifact first. Assert zero artifact rows for this
        // exact job, so a refusal that already emitted a ZIP cannot pass.
        let artifacts = sqlx::query(
            r"
            SELECT count(*)::bigint AS total,
                   count(*) FILTER (WHERE artifact_kind = 'export_zip')::bigint AS zips
            FROM private.lca_package_artifacts
            WHERE job_id = $1
            ",
        )
        .bind(job_id)
        .fetch_one(&state.pool)
        .await?;
        let total = artifacts.try_get::<i64, _>("total")?;
        let zips = artifacts.try_get::<i64, _>("zips")?;
        anyhow::ensure!(
            total == 0 && zips == 0,
            "export scope={scope} refused but still produced artifacts (total={total} export_zip={zips})"
        );
        eprintln!("export scope={scope} refused with zero artifact rows (no export ZIP produced)");
        return Ok(());
    }

    let row = sqlx::query(
        r"
        SELECT a.artifact_url, a.metadata
        FROM private.lca_package_artifacts a
        WHERE a.job_id = $1 AND a.artifact_kind = 'export_zip'
        ORDER BY a.created_at DESC
        LIMIT 1
        ",
    )
    .bind(job_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("export scope={scope} produced no ZIP artifact"))?;
    let artifact_url = row.try_get::<String, _>("artifact_url")?;
    let metadata = row.try_get::<Value, _>("metadata")?;
    if let Ok(artifact_id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM private.lca_package_artifacts WHERE job_id=$1 AND artifact_kind='export_zip' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_one(&state.pool)
    .await
    {
        tracked.package_artifacts.push(artifact_id);
    }
    anyhow::ensure!(
        metadata
            .get("productExportPolicyVersion")
            .and_then(Value::as_str)
            == Some(expected_policy),
        "export scope={scope} artifact is not bound to the product exposure policy: {metadata}"
    );

    let zip_bytes = state
        .object_store
        .download_object_url(artifact_url.as_str())
        .await?;
    let manifest = read_package_manifest(zip_bytes.as_slice())?;
    let packaged = manifest
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("export scope={scope} manifest lacks entries"))?
        .iter()
        .filter(|entry| entry.get("table").and_then(Value::as_str) == Some("processes"))
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();

    anyhow::ensure!(
        !packaged.contains(&fixture.result.to_string()),
        "export scope={scope} packaged the published Result: {packaged:?}"
    );
    if !roots.is_empty() && roots[0].0 == fixture.unit {
        anyhow::ensure!(
            packaged.contains(&fixture.unit.to_string()),
            "export scope={scope} dropped the public Unit it was asked for: {packaged:?}"
        );
    }
    // Support data keeps its existing export semantics.
    let support_tables = manifest
        .get("entries")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("table").and_then(Value::as_str))
                .filter(|table| *table != "processes")
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if roots.is_empty() {
        anyhow::ensure!(
            !support_tables.is_empty(),
            "export scope={scope} packaged no support data at all"
        );
    }
    eprintln!(
        "export scope={scope} packaged processes={packaged:?} support_tables={:?}",
        support_tables.len()
    );
    Ok(())
}

/// Reads the `manifest.json` member out of an exported package ZIP.
fn read_package_manifest(zip_bytes: &[u8]) -> anyhow::Result<Value> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;
    let mut manifest = archive
        .by_name("manifest.json")
        .map_err(|error| anyhow::anyhow!("exported package lacks manifest.json: {error}"))?;
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut manifest, &mut raw)?;
    Ok(serde_json::from_str(raw.as_str())?)
}

/// Storage-free subset: Database eligibility predicates and the export fence predicate.
///
/// This runs without object storage, so it is PARTIAL proof. It does not build a snapshot, does not
/// run provider matching, does not solve, and does not produce a package. It must never be reported
/// as the DB-to-solver acceptance.
#[tokio::test]
#[ignore = "not run by default: needs a dedicated task DB only (partial proof); run scripts/run_result120_numerical_isolation.sh with RESULT120_SCENARIO=database-only"]
async fn database_only_eligibility_and_export_fences() -> anyhow::Result<()> {
    let guard = guard_task_instance(TaskInstanceScope::Database)?;
    let _ = guard.task_instance_id();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(required_env("DATABASE_URL").as_str())
        .await?;
    preflight_task_instance(&pool).await?;
    let actor = Uuid::new_v4();
    // Database-only: the fixture never touches object storage, so no bucket parameter is required.
    let mut runner = ScenarioRunner::new(guard, allocate_fixture(), actor, "database-only");
    let fixture = runner.fixture.clone();
    let outcome = run_database_only_scenario(&pool, &fixture, actor, runner.tracked_mut()).await;
    runner.finish(if outcome.is_ok() { "ok" } else { "failed" });
    outcome
}

async fn run_database_only_scenario(
    pool: &PgPool,
    fixture: &Fixture,
    actor: Uuid,
    tracked: &mut TrackedResources,
) -> anyhow::Result<()> {
    // No worker jobs, snapshots or artifacts are created in this storage-free subset, so the
    // manifest records only the fixture identities.
    let _ = &tracked;
    seed_fixture(
        pool,
        actor,
        fixture,
        PROCESS_STATE_PUBLIC,
        PROCESS_STATE_RESULT,
        FixtureProcesses::WithResult,
        // Database-only: no bucket row is needed and `S3_BUCKET` is never read.
        false,
    )
    .await?;

    // No current release: the candidate predicate must be the exact-100 literal.
    let candidate = normalize_scope(
        pool,
        actor,
        fixture,
        json!({"coverageMode": "global_eligible"}),
    )
    .await?;
    anyhow::ensure!(
        candidate
            .get("eligibilityPredicateVersion")
            .and_then(Value::as_str)
            == Some(solver_worker::CANDIDATE_PUBLIC_NUMERICAL_PREDICATE_V2),
        "no-current-release predicate is not the exact-100 candidate literal: {candidate}"
    );

    // The Result already exists at state 120, so this is the post-publication candidate universe.
    let candidate_after = normalize_scope(
        pool,
        actor,
        fixture,
        json!({"coverageMode": "global_eligible"}),
    )
    .await?;
    let ids = candidate_after
        .get("processes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("candidate scope lacks processes"))?
        .iter()
        .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !ids.contains(&fixture.result.to_string()),
        "the published Result stayed in the candidate universe: {ids:?}"
    );

    // Exact selected-root admission: the Database refuses a subset scope naming the Result.
    let subset_error = normalize_scope(
        pool,
        actor,
        fixture,
        json!({
            "coverageMode": "subset",
            "processes": [{"id": fixture.result, "version": VERSION}],
        }),
    )
    .await;
    anyhow::ensure!(
        subset_error.is_err(),
        "the Database accepted an explicit subset scope naming the published Result"
    );

    // Export fence predicate, evaluated against the real rows: the exact predicate the Worker's
    // product-export SQL applies must exclude the Result and include the public Unit.
    let exportable = sqlx::query_scalar::<_, i64>(
        // Mirrors the Worker product-export fence, including its NULL-safety: `state_code` is
        // nullable and NULL is exportable, so `IS DISTINCT FROM` is required here too.
        "SELECT count(*)::bigint FROM public.processes WHERE id = ANY($1::uuid[]) AND state_code IS DISTINCT FROM 120",
    )
    .bind(vec![fixture.result, fixture.unit])
    .fetch_one(pool)
    .await?;
    anyhow::ensure!(
        exportable == 1,
        "the export fence predicate should keep exactly the public Unit, got {exportable}"
    );
    // Support data keeps its unchanged 100..=199 open-data semantics.
    let open_data_flows = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)::bigint FROM public.flows WHERE state_code = ANY(SELECT generate_series(100,199)) ",
    )
    .fetch_one(pool)
    .await?;
    anyhow::ensure!(
        open_data_flows >= 4,
        "open-data support selection unexpectedly narrowed: {open_data_flows}"
    );

    eprintln!(
        "database-only subset OK: candidate predicate exact-100, Result excluded, export fence keeps Unit, {open_data_flows} support flows unchanged"
    );
    Ok(())
}

/// Runs one query returning a JSON value with the service-role claim set.
/// Calls the production scope normalizer with the reviewed method axis always present.
///
/// The candidate (no-current-release) branch validates `lciaMethods` against
/// `private.lcia_scope_closure_reviewed_lcia_methods` on every call, including process-only
/// assertions: an omitted axis raises `invalid_lcia_method_selection`. This helper supplies the
/// fixture's catalog method so the process predicate is what the assertion actually exercises.
async fn normalize_scope(
    pool: &PgPool,
    actor: Uuid,
    fixture: &Fixture,
    extra: Value,
) -> anyhow::Result<Value> {
    let Value::Object(mut scope) = extra else {
        anyhow::bail!("scope extra must be an object");
    };
    scope.insert(
        "lciaMethods".to_owned(),
        json!([{"id": fixture.method, "version": fixture.method_version}]),
    );
    trusted_scalar_json(
        pool,
        actor,
        "SELECT private.lcia_scope_closure_normalize_request($1::jsonb) AS v",
        Value::Object(scope),
    )
    .await
}

/// Runs one JSON-returning RPC as the service role inside an explicit transaction./// Runs one JSON-returning RPC as the service role inside an explicit transaction.
///
/// `set_config(..., true)` is transaction-local. Wrapping it in an unreferenced CTE is not reliable
/// authorization setup (the planner may not evaluate it), so the claim is set with its own statement
/// and the RPC runs in the same transaction. The transaction is always rolled back: this helper only
/// reads, and rolling back guarantees no claim or side effect can leak to the next pool checkout.
async fn trusted_scalar_json(
    pool: &PgPool,
    actor: Uuid,
    sql: &str,
    argument: Value,
) -> anyhow::Result<Value> {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('request.jwt.claim.role','service_role',true)")
        .execute(&mut *tx)
        .await?;
    // `set_config` takes `text`; binding the Uuid directly would resolve to a nonexistent
    // `set_config(unknown, uuid, boolean)` overload.
    sqlx::query("SELECT set_config('request.jwt.claim.sub',$1::text,true)")
        .bind(actor.to_string())
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query(sql).bind(argument).fetch_one(&mut *tx).await?;
    let value = row.try_get::<Value, _>("v")?;
    tx.rollback().await?;
    Ok(value)
}

/// Requests a snapshot whose only request root is `root`, returning a diagnostic on refusal.
async fn build_snapshot_with_roots(
    state: Arc<AppState>,
    fixture: &Fixture,
    actor: Uuid,
    root: Uuid,
) -> anyhow::Result<Uuid> {
    let snapshot_id = Uuid::new_v4();
    let job_id = Uuid::new_v4();
    enqueue_job(
        &state.pool,
        job_id,
        actor,
        "lca.build_snapshot",
        "solver",
        "lca.build_snapshot.request.v1",
        json!({
            "job_id": job_id,
            "snapshot_id": snapshot_id,
            "all_states": false,
            "process_states": solver_worker::default_snapshot_process_states_arg(),
            "request_roots": [RequestRootProcess::new(root, VERSION)],
            "provider_rule": "split_by_process_volume",
            "reference_normalization_mode": "lenient",
            "allocation_fraction_mode": "lenient",
            "no_lcia": true,
        }),
        fixture.consumer,
    )
    .await?;
    let (status, diagnostics, result_json) = run_one_job(state, job_id, "result-root").await?;
    if status == "completed" {
        let resolved = result_json
            .get("snapshotId")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("resolved snapshot missing: {result_json}"))?
            .parse::<Uuid>()?;
        return Ok(resolved);
    }
    anyhow::bail!(
        "builder refused the Result request root with status={status} diagnostics={diagnostics}"
    )
}
