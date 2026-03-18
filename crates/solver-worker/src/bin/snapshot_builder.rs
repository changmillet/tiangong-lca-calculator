#![allow(
    clippy::cast_precision_loss,
    clippy::collapsible_if,
    clippy::comparison_chain,
    clippy::format_push_string,
    clippy::needless_raw_string_hashes,
    clippy::reserve_after_initialization,
    clippy::struct_field_names,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use clap::Parser;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use solver_core::{ModelSparseData, SparseTriplet};
use solver_worker::snapshot_artifacts::{
    SNAPSHOT_ARTIFACT_FORMAT, SnapshotAllocationCoverage, SnapshotBuildConfig,
    SnapshotCoverageReport, SnapshotMatchingCoverage, SnapshotMatrixScale,
    SnapshotReferenceCoverage, SnapshotSingularRisk, encode_snapshot_artifact,
};
use solver_worker::snapshot_index::{
    SnapshotImpactMapEntry, SnapshotIndexDocument, SnapshotProcessMapEntry,
};
use solver_worker::storage::ObjectStoreClient;
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use uuid::Uuid;

#[derive(Debug, Clone, Parser)]
#[command(name = "snapshot-builder")]
struct Cli {
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,
    #[arg(long, env = "CONN")]
    conn: Option<String>,
    #[arg(long, env = "S3_ENDPOINT")]
    s3_endpoint: Option<String>,
    #[arg(long, env = "S3_REGION")]
    s3_region: Option<String>,
    #[arg(long, env = "S3_BUCKET")]
    s3_bucket: Option<String>,
    #[arg(long, env = "S3_ACCESS_KEY_ID")]
    s3_access_key_id: Option<String>,
    #[arg(long, env = "S3_SECRET_ACCESS_KEY")]
    s3_secret_access_key: Option<String>,
    #[arg(long, env = "S3_SESSION_TOKEN")]
    s3_session_token: Option<String>,
    #[arg(long, env = "S3_PREFIX", default_value = "lca-results")]
    s3_prefix: String,
    #[arg(long)]
    snapshot_id: Option<Uuid>,
    #[arg(long, default_value = "100")]
    process_states: String,
    #[arg(long)]
    include_user_id: Option<Uuid>,
    #[arg(long, default_value_t = 0)]
    process_limit: usize,
    #[arg(long, default_value = "strict_unique_provider")]
    provider_rule: String,
    #[arg(long, default_value = "strict")]
    reference_normalization_mode: String,
    #[arg(long, default_value = "strict")]
    allocation_fraction_mode: String,
    #[arg(long, default_value_t = 0.999_999)]
    self_loop_cutoff: f64,
    #[arg(long, default_value_t = 1e-12)]
    singular_eps: f64,
    #[arg(long)]
    method_id: Option<Uuid>,
    #[arg(long)]
    method_version: Option<String>,
    #[arg(long)]
    no_lcia: bool,
    #[arg(long, default_value = "reports/snapshot-coverage")]
    report_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct ProcessRow {
    id: Uuid,
    version: String,
    model_id: Option<Uuid>,
    json: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExchangeDirection {
    Input,
    Output,
}

#[derive(Debug, Clone)]
struct ParsedExchange {
    process_idx: i32,
    flow_id: Uuid,
    direction: ExchangeDirection,
    amount: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderRule {
    StrictUniqueProvider,
    BestProviderStrict,
    SplitByEvidenceStrict,
    SplitByEvidenceHybrid,
    SplitEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NormalizationMode {
    Strict,
    Lenient,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocationMode {
    Strict,
    Lenient,
}

#[derive(Debug, Clone)]
struct ProcessMeta {
    process_idx: i32,
    process_id: Uuid,
    process_version: String,
    process_name: Option<String>,
    model_id: Option<Uuid>,
    location: Option<String>,
    reference_year: Option<i32>,
}

#[derive(Debug, Clone)]
struct ProviderCandidateScore {
    provider_idx: i32,
    provider_id: Uuid,
    geo_score: f64,
    time_score: f64,
    final_score: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ReferenceParseStats {
    missing_reference: i64,
    invalid_reference: i64,
    normalized_processes: i64,
}

#[derive(Debug, Clone, Copy, Default)]
struct AllocationParseStats {
    exchange_total: i64,
    fraction_present_count: i64,
    fraction_missing_count: i64,
    fraction_invalid_count: i64,
}

#[derive(Debug, Clone)]
struct MethodSelection {
    has_lcia: bool,
    method_id: Option<Uuid>,
    method_version: Option<String>,
    method_count: i64,
    factor_count: i64,
}

#[derive(Debug, Clone)]
struct ImpactFactorSet {
    impact_id: Uuid,
    impact_key: String,
    impact_name: String,
    unit: String,
    factors_by_flow: HashMap<Uuid, f64>,
}

#[derive(Debug, Clone)]
struct BuildOutput {
    data: ModelSparseData,
    coverage: SnapshotCoverageReport,
    snapshot_index: SnapshotIndexDocument,
}

type ParsedProcessChunk = (
    ProcessMeta,
    Vec<ParsedExchange>,
    BTreeSet<Uuid>,
    ReferenceParseStats,
    AllocationParseStats,
);

#[derive(Debug, Clone, Serialize)]
struct SourceSnapshotSummary {
    process_count: i64,
    process_max_modified_at_utc: String,
    flow_count: i64,
    flow_max_modified_at_utc: String,
    lciamethod_count: i64,
    lciamethod_max_modified_at_utc: String,
}

#[derive(Debug, Clone)]
struct ReuseCandidate {
    snapshot_id: Uuid,
    artifact_url: String,
    artifact_sha256: String,
    artifact_byte_size: i64,
    artifact_format: String,
    coverage: SnapshotCoverageReport,
    process_count: i64,
    flow_count: i64,
    impact_count: i64,
    a_nnz: i64,
    b_nnz: i64,
    c_nnz: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct BuildTimingSec {
    reused_snapshot: bool,
    total_sec: f64,
    resolve_method_identity_sec: f64,
    compute_source_fingerprint_sec: f64,
    reuse_lookup_sec: f64,
    load_method_factors_sec: f64,
    build_sparse_payload_sec: f64,
    encode_artifact_sec: f64,
    upload_artifact_sec: f64,
    upload_snapshot_index_sec: f64,
    persist_metadata_sec: f64,
}

impl ProviderRule {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "strict_unique_provider" => Ok(Self::StrictUniqueProvider),
            "best_provider_strict" => Ok(Self::BestProviderStrict),
            "split_by_evidence" => Ok(Self::SplitByEvidenceStrict),
            "split_by_evidence_hybrid" => Ok(Self::SplitByEvidenceHybrid),
            "split_equal" => Ok(Self::SplitEqual),
            _ => Err(anyhow::anyhow!(
                "unsupported provider_rule={value}; expected one of: strict_unique_provider, best_provider_strict, split_by_evidence, split_by_evidence_hybrid, split_equal"
            )),
        }
    }
}

impl NormalizationMode {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "strict" => Ok(Self::Strict),
            "lenient" => Ok(Self::Lenient),
            _ => Err(anyhow::anyhow!(
                "unsupported reference_normalization_mode={value}; expected strict or lenient"
            )),
        }
    }
}

impl AllocationMode {
    fn parse(value: &str) -> anyhow::Result<Self> {
        match value {
            "strict" => Ok(Self::Strict),
            "lenient" => Ok(Self::Lenient),
            _ => Err(anyhow::anyhow!(
                "unsupported allocation_fraction_mode={value}; expected strict or lenient"
            )),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let total_started = Instant::now();
    let cli = Cli::parse();
    let provider_rule = ProviderRule::parse(&cli.provider_rule)?;
    let reference_normalization_mode = NormalizationMode::parse(&cli.reference_normalization_mode)?;
    let allocation_mode = AllocationMode::parse(&cli.allocation_fraction_mode)?;

    let db_url = cli
        .database_url
        .as_deref()
        .or(cli.conn.as_deref())
        .ok_or_else(|| anyhow::anyhow!("missing DB connection: set DATABASE_URL or CONN"))?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET statement_timeout = 0")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(db_url)
        .await?;

    let store = build_object_store(&cli)?;
    let requested_snapshot_id = cli.snapshot_id;
    let (all_states, state_codes, process_states_label) =
        parse_process_states(&cli.process_states)?;
    let mut build_timing = BuildTimingSec::default();
    let method_started = Instant::now();
    let method = resolve_method_identity(&pool, &cli).await?;
    build_timing.resolve_method_identity_sec = method_started.elapsed().as_secs_f64();
    let build_config = SnapshotBuildConfig {
        process_states: process_states_label.clone(),
        include_user_id: cli.include_user_id,
        process_limit: i32::try_from(cli.process_limit)
            .map_err(|_| anyhow::anyhow!("process_limit overflow"))?,
        provider_rule: cli.provider_rule.clone(),
        reference_normalization_mode: cli.reference_normalization_mode.clone(),
        allocation_fraction_mode: cli.allocation_fraction_mode.clone(),
        biosphere_sign_mode: "gross".to_owned(),
        self_loop_cutoff: cli.self_loop_cutoff,
        singular_eps: cli.singular_eps,
        has_lcia: method.has_lcia,
        method_id: method.method_id,
        method_version: method.method_version.clone(),
    };
    let fingerprint_started = Instant::now();
    let (source_summary, source_fingerprint) = compute_source_fingerprint(
        &pool,
        all_states,
        &state_codes,
        cli.include_user_id,
        cli.process_limit,
        &build_config,
    )
    .await?;
    build_timing.compute_source_fingerprint_sec = fingerprint_started.elapsed().as_secs_f64();

    if let Some(snapshot_id) = requested_snapshot_id {
        println!("[info] snapshot_id={snapshot_id} (requested)");
    } else {
        println!("[info] snapshot_id=auto");
    }
    println!("[info] process_states={process_states_label}");
    if let Some(include_user_id) = cli.include_user_id {
        println!("[info] include_user_id={include_user_id}");
    } else {
        println!("[info] include_user_id=none");
    }
    println!("[info] process_limit={}", cli.process_limit);
    println!("[info] provider_rule={}", cli.provider_rule);
    println!(
        "[info] reference_normalization_mode={}",
        cli.reference_normalization_mode
    );
    println!(
        "[info] allocation_fraction_mode={}",
        cli.allocation_fraction_mode
    );
    println!("[info] biosphere_sign_mode=gross");
    println!("[info] self_loop_cutoff={}", cli.self_loop_cutoff);
    println!("[info] singular_eps={}", cli.singular_eps);
    if method.has_lcia {
        if let Some(method_id) = method.method_id {
            println!(
                "[info] lcia_method={}@{} factors={}",
                method_id,
                method.method_version.as_deref().unwrap_or_default(),
                method.factor_count
            );
        } else {
            println!(
                "[info] lcia_method=all methods={} factors={}",
                method.method_count, method.factor_count
            );
        }
    } else {
        println!("[info] lcia_method=disabled");
    }
    println!(
        "[source] processes={} max_modified_at={} flows={} max_modified_at={} lciamethods={} max_modified_at={}",
        source_summary.process_count,
        source_summary.process_max_modified_at_utc,
        source_summary.flow_count,
        source_summary.flow_max_modified_at_utc,
        source_summary.lciamethod_count,
        source_summary.lciamethod_max_modified_at_utc
    );
    println!("[source] fingerprint={source_fingerprint}");

    let reuse_lookup_started = Instant::now();
    let reused_candidate = find_reusable_snapshot(&pool, &source_fingerprint).await?;
    build_timing.reuse_lookup_sec = reuse_lookup_started.elapsed().as_secs_f64();

    if let Some(reused) = reused_candidate {
        let snapshot_index_url = derive_snapshot_index_url(&reused.artifact_url);
        match store.download_object_url(&snapshot_index_url).await {
            Ok(_) => {
                let resolved_snapshot_id = if let Some(requested) = requested_snapshot_id {
                    if requested != reused.snapshot_id {
                        persist_reused_snapshot_metadata(
                            &pool,
                            requested,
                            &cli.provider_rule,
                            all_states,
                            &state_codes,
                            cli.include_user_id,
                            &source_fingerprint,
                            &method,
                            &reused,
                        )
                        .await?;
                    }
                    requested
                } else {
                    reused.snapshot_id
                };

                build_timing.reused_snapshot = true;
                build_timing.total_sec = total_started.elapsed().as_secs_f64();
                write_report_files(
                    &cli.report_dir,
                    resolved_snapshot_id,
                    &build_config,
                    &reused.coverage,
                    &reused.artifact_url,
                    &source_summary,
                    &source_fingerprint,
                    &build_timing,
                )?;
                println!(
                    "[reuse] matched existing ready snapshot={}",
                    reused.snapshot_id
                );
                println!("[done] snapshot ready: {resolved_snapshot_id}");
                println!("[artifact] {}", reused.artifact_url);
                println!("[snapshot_index] {snapshot_index_url}");
                println!(
                    "[matrix] process_count={} flow_count={} impact_count={} a_nnz={} b_nnz={} c_nnz={}",
                    reused.process_count,
                    reused.flow_count,
                    reused.impact_count,
                    reused.a_nnz,
                    reused.b_nnz,
                    reused.c_nnz
                );
                println!(
                    "[coverage] unique_match={} any_match={} singular_risk={}",
                    reused.coverage.matching.unique_provider_match_pct,
                    reused.coverage.matching.any_provider_match_pct,
                    reused.coverage.singular_risk.risk_level
                );
                return Ok(());
            }
            Err(error) => {
                println!(
                    "[reuse] skip snapshot={} because snapshot index sidecar is unavailable: {}",
                    reused.snapshot_id, error
                );
            }
        }
    }

    let factor_map_started = Instant::now();
    let impact_factor_sets = load_impact_factor_sets(&pool, &method).await?;
    build_timing.load_method_factors_sec = factor_map_started.elapsed().as_secs_f64();

    let snapshot_id = requested_snapshot_id.unwrap_or_else(Uuid::new_v4);
    let build_started = Instant::now();
    let built = build_sparse_payload(
        &pool,
        snapshot_id,
        &method,
        all_states,
        &state_codes,
        cli.include_user_id,
        cli.process_limit,
        provider_rule,
        reference_normalization_mode,
        allocation_mode,
        cli.self_loop_cutoff,
        cli.singular_eps,
        method.has_lcia,
        &impact_factor_sets,
    )
    .await?;
    build_timing.build_sparse_payload_sec = build_started.elapsed().as_secs_f64();

    let encode_started = Instant::now();
    let encoded = encode_snapshot_artifact(
        snapshot_id,
        build_config.clone(),
        built.coverage.clone(),
        &built.data,
    )?;
    build_timing.encode_artifact_sec = encode_started.elapsed().as_secs_f64();

    let upload_started = Instant::now();
    let artifact_url = store
        .upload_snapshot_artifact(
            snapshot_id,
            encoded.extension,
            encoded.content_type,
            encoded.bytes,
        )
        .await?;
    build_timing.upload_artifact_sec = upload_started.elapsed().as_secs_f64();

    let snapshot_index_bytes = serde_json::to_vec(&built.snapshot_index)?;
    let upload_snapshot_index_started = Instant::now();
    let snapshot_index_url = store
        .upload_snapshot_index(snapshot_id, snapshot_index_bytes)
        .await?;
    build_timing.upload_snapshot_index_sec = upload_snapshot_index_started.elapsed().as_secs_f64();

    let persist_started = Instant::now();
    persist_snapshot_metadata(
        &pool,
        snapshot_id,
        &cli.provider_rule,
        all_states,
        &state_codes,
        cli.include_user_id,
        &source_fingerprint,
        &method,
        &built,
        &artifact_url,
        &encoded.sha256,
        i64::try_from(encoded.byte_size).map_err(|_| anyhow::anyhow!("artifact too large"))?,
        encoded.format,
    )
    .await?;
    build_timing.persist_metadata_sec = persist_started.elapsed().as_secs_f64();
    build_timing.total_sec = total_started.elapsed().as_secs_f64();

    write_report_files(
        &cli.report_dir,
        snapshot_id,
        &build_config,
        &built.coverage,
        &artifact_url,
        &source_summary,
        &source_fingerprint,
        &build_timing,
    )?;

    println!("[done] snapshot ready: {snapshot_id}");
    println!("[artifact] {artifact_url}");
    println!("[snapshot_index] {snapshot_index_url}");
    println!(
        "[matrix] process_count={} flow_count={} a_nnz={} b_nnz={} c_nnz={}",
        built.data.process_count,
        built.data.flow_count,
        built.coverage.matrix_scale.a_nnz,
        built.coverage.matrix_scale.b_nnz,
        built.coverage.matrix_scale.c_nnz
    );
    println!(
        "[coverage] unique_match={} any_match={} singular_risk={}",
        built.coverage.matching.unique_provider_match_pct,
        built.coverage.matching.any_provider_match_pct,
        built.coverage.singular_risk.risk_level
    );

    Ok(())
}

fn build_object_store(cli: &Cli) -> anyhow::Result<ObjectStoreClient> {
    let endpoint = cli
        .s3_endpoint
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing S3_ENDPOINT"))?;
    let region = cli
        .s3_region
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing S3_REGION"))?;
    let bucket = cli
        .s3_bucket
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing S3_BUCKET"))?;
    let access_key_id = cli
        .s3_access_key_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing S3_ACCESS_KEY_ID"))?;
    let secret_access_key = cli
        .s3_secret_access_key
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("missing S3_SECRET_ACCESS_KEY"))?;

    ObjectStoreClient::new(
        endpoint,
        region,
        bucket,
        &cli.s3_prefix,
        access_key_id,
        secret_access_key,
        cli.s3_session_token.clone(),
    )
}

fn parse_process_states(input: &str) -> anyhow::Result<(bool, Vec<i32>, String)> {
    let trimmed = input.trim().replace(' ', "");
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
        return Ok((true, Vec::new(), "all".to_owned()));
    }

    let mut out = Vec::new();
    for token in trimmed.split(',') {
        let value: i32 = token
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid process state code: {token}"))?;
        out.push(value);
    }
    out.sort_unstable();
    out.dedup();
    let label = out.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
    Ok((false, out, label))
}

fn derive_snapshot_index_url(artifact_url: &str) -> String {
    match artifact_url.rfind('/') {
        Some(idx) => format!("{}snapshot-index-v1.json", &artifact_url[..=idx]),
        None => format!("{artifact_url}/snapshot-index-v1.json"),
    }
}

async fn resolve_method_identity(pool: &PgPool, cli: &Cli) -> anyhow::Result<MethodSelection> {
    if cli.no_lcia {
        return Ok(MethodSelection {
            has_lcia: false,
            method_id: None,
            method_version: None,
            method_count: 0,
            factor_count: 0,
        });
    }

    if cli.method_id.is_some() && cli.method_version.is_none() {
        return Err(anyhow::anyhow!(
            "--method-version is required when --method-id is set"
        ));
    }
    if cli.method_id.is_none() && cli.method_version.is_some() {
        return Err(anyhow::anyhow!(
            "--method-version requires --method-id; omit both to use all methods"
        ));
    }

    if let Some(method_id) = cli.method_id {
        let method_version = cli
            .method_version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing method version"))?;
        let factor_count = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT
              CASE
                WHEN jsonb_typeof(json#>'{LCIAMethodDataSet,characterisationFactors,factor}') = 'array'
                THEN jsonb_array_length(json#>'{LCIAMethodDataSet,characterisationFactors,factor}')
                ELSE 0
              END::bigint AS factor_cnt
            FROM public.lciamethods
            WHERE id = $1
              AND version = $2::bpchar
            "#,
        )
        .bind(method_id)
        .bind(method_version.clone())
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| anyhow::anyhow!("lciamethod not found: {method_id}@{method_version}"))?;
        return Ok(MethodSelection {
            has_lcia: true,
            method_id: Some(method_id),
            method_version: Some(method_version),
            method_count: 1,
            factor_count,
        });
    }

    let row = sqlx::query(
        r#"
        WITH latest AS (
          SELECT DISTINCT ON (id)
            id,
            json
          FROM public.lciamethods
          ORDER BY id, state_code DESC, modified_at DESC NULLS LAST, created_at DESC NULLS LAST
        )
        SELECT
          COUNT(*)::bigint AS method_cnt,
          COALESCE(
            SUM(
              CASE
                WHEN jsonb_typeof(json#>'{LCIAMethodDataSet,characterisationFactors,factor}') = 'array'
                THEN jsonb_array_length(json#>'{LCIAMethodDataSet,characterisationFactors,factor}')
                ELSE 0
              END
            ),
            0
          )::bigint AS factor_cnt
        FROM latest
        "#,
    )
    .fetch_optional(pool)
    .await?;

    let row = row.ok_or_else(|| anyhow::anyhow!("no lciamethods found"))?;
    let method_count = row.try_get::<i64, _>("method_cnt")?;
    if method_count <= 0 {
        return Err(anyhow::anyhow!("no lciamethods found"));
    }
    let factor_count = row.try_get::<i64, _>("factor_cnt")?;

    Ok(MethodSelection {
        has_lcia: true,
        method_id: None,
        method_version: None,
        method_count,
        factor_count,
    })
}

fn parse_lang_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        Value::Object(map) => {
            if let Some(text) = map.get("#text").and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
            None
        }
        Value::Array(arr) => {
            let preferred = arr.iter().find(|entry| {
                entry
                    .get("@xml:lang")
                    .and_then(Value::as_str)
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("en"))
            });
            if let Some(entry) = preferred
                && let Some(text) = parse_lang_text(entry)
            {
                return Some(text);
            }
            arr.iter().find_map(parse_lang_text)
        }
        _ => None,
    }
}

fn parse_string_path(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    parse_lang_text(current)
}

fn parse_lcia_method_name(method_json: &Value) -> Option<String> {
    parse_string_path(
        method_json,
        &[
            "LCIAMethodDataSet",
            "methodInformation",
            "dataSetInformation",
            "name",
            "baseName",
        ],
    )
    .or_else(|| {
        parse_string_path(
            method_json,
            &[
                "LCIAMethodDataSet",
                "methodInfo",
                "dataSetInfo",
                "name",
                "baseName",
            ],
        )
    })
}

fn parse_lcia_method_unit(method_json: &Value) -> Option<String> {
    parse_string_path(
        method_json,
        &[
            "LCIAMethodDataSet",
            "methodInformation",
            "quantitativeReference",
            "referenceToReferenceUnitGroup",
            "common:shortDescription",
        ],
    )
    .or_else(|| {
        parse_string_path(
            method_json,
            &[
                "LCIAMethodDataSet",
                "methodInfo",
                "quantitativeReference",
                "referenceToReferenceUnitGroup",
                "common:shortDescription",
            ],
        )
    })
}

async fn load_impact_factor_sets(
    pool: &PgPool,
    method: &MethodSelection,
) -> anyhow::Result<Vec<ImpactFactorSet>> {
    if !method.has_lcia {
        return Ok(Vec::new());
    }

    let rows = if let Some(method_id) = method.method_id {
        let method_version = method
            .method_version
            .clone()
            .ok_or_else(|| anyhow::anyhow!("missing method version"))?;
        sqlx::query(
            r#"
            SELECT id, json
            FROM public.lciamethods
            WHERE id = $1
              AND version = $2::bpchar
            "#,
        )
        .bind(method_id)
        .bind(method_version)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT DISTINCT ON (id)
              id,
              json
            FROM public.lciamethods
            ORDER BY id, state_code DESC, modified_at DESC NULLS LAST, created_at DESC NULLS LAST
            "#,
        )
        .fetch_all(pool)
        .await?
    };

    if rows.is_empty() {
        return Err(anyhow::anyhow!("no lciamethods found for selected scope"));
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let method_id = row.try_get::<Uuid, _>("id")?;
        let method_json = row.try_get::<Value, _>("json")?;

        let mut factor_map: HashMap<Uuid, f64> = HashMap::new();
        for factor in method_factor_items(&method_json) {
            let Some(flow_id) = parse_uuid_at(factor, &["referenceToFlowDataSet", "@refObjectId"])
            else {
                continue;
            };
            let Some(value) = parse_number(
                factor
                    .get("meanValue")
                    .or_else(|| factor.get("meanAmount"))
                    .or_else(|| factor.get("resultingAmount")),
            ) else {
                continue;
            };
            if value.abs() <= f64::EPSILON {
                continue;
            }
            *factor_map.entry(flow_id).or_insert(0.0) += value;
        }
        factor_map.retain(|_, value| value.abs() > f64::EPSILON);

        out.push(ImpactFactorSet {
            impact_id: method_id,
            impact_key: format!("method:{method_id}"),
            impact_name: parse_lcia_method_name(&method_json)
                .unwrap_or_else(|| format!("LCIA Method {method_id}")),
            unit: parse_lcia_method_unit(&method_json).unwrap_or_else(|| "unknown".to_owned()),
            factors_by_flow: factor_map,
        });
    }
    out.sort_unstable_by_key(|impact| impact.impact_id);
    Ok(out)
}

fn add_technosphere_edge(
    a_map: &mut HashMap<(i32, i32), f64>,
    provider_idx: i32,
    consumer_idx: i32,
    amount: f64,
) {
    if amount.abs() > f64::EPSILON {
        *a_map.entry((provider_idx, consumer_idx)).or_insert(0.0) += amount;
    }
}

async fn build_sparse_payload(
    pool: &PgPool,
    snapshot_id: Uuid,
    method: &MethodSelection,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
    process_limit: usize,
    provider_rule: ProviderRule,
    reference_normalization_mode: NormalizationMode,
    allocation_mode: AllocationMode,
    self_loop_cutoff: f64,
    singular_eps: f64,
    has_lcia: bool,
    impact_factor_sets: &[ImpactFactorSet],
) -> anyhow::Result<BuildOutput> {
    let mut processes = fetch_processes(pool, all_states, state_codes, include_user_id).await?;
    if process_limit > 0 && processes.len() > process_limit {
        processes.truncate(process_limit);
    }
    if processes.is_empty() {
        return Err(anyhow::anyhow!("no processes matched filter"));
    }
    if has_lcia && impact_factor_sets.is_empty() {
        return Err(anyhow::anyhow!(
            "LCIA is enabled but no lciamethod factors were loaded"
        ));
    }
    let process_count_i32 =
        i32::try_from(processes.len()).map_err(|_| anyhow::anyhow!("process overflow"))?;

    let chunks = processes
        .par_iter()
        .enumerate()
        .map(|(idx, proc_row)| -> anyhow::Result<ParsedProcessChunk> {
            let process_idx =
                i32::try_from(idx).map_err(|_| anyhow::anyhow!("process index overflow"))?;
            let process_meta = ProcessMeta {
                process_idx,
                process_id: proc_row.id,
                process_version: proc_row.version.clone(),
                process_name: parse_process_name(&proc_row.json),
                model_id: proc_row.model_id,
                location: parse_process_location(&proc_row.json),
                reference_year: parse_process_reference_year(&proc_row.json),
            };
            let mut local_exchanges = Vec::new();
            let mut local_flow_ids = BTreeSet::new();
            let mut local_allocation = AllocationParseStats::default();
            let exchange_items = process_exchange_items(&proc_row.json);
            let (reference_scale, local_reference) = resolve_reference_normalization(
                proc_row.id,
                &proc_row.json,
                &exchange_items,
                reference_normalization_mode,
            )?;

            for ex in &exchange_items {
                let direction = match ex
                    .get("exchangeDirection")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "Input" => Some(ExchangeDirection::Input),
                    "Output" => Some(ExchangeDirection::Output),
                    _ => None,
                };
                let Some(direction) = direction else {
                    continue;
                };
                let Some(flow_id) = parse_uuid_at(ex, &["referenceToFlowDataSet", "@refObjectId"])
                else {
                    continue;
                };
                local_allocation.exchange_total += 1;
                let (allocation_fraction, allocation_state) =
                    resolve_allocation_fraction(ex, allocation_mode)?;
                match allocation_state {
                    AllocationFractionState::Present => {
                        local_allocation.fraction_present_count += 1;
                    }
                    AllocationFractionState::Missing => {
                        local_allocation.fraction_missing_count += 1;
                    }
                    AllocationFractionState::Invalid => {
                        local_allocation.fraction_invalid_count += 1;
                    }
                }
                let amount = parse_number(
                    ex.get("meanAmount")
                        .or_else(|| ex.get("resultingAmount"))
                        .or_else(|| ex.get("meanValue")),
                )
                .map(|raw| raw * reference_scale * allocation_fraction);

                local_exchanges.push(ParsedExchange {
                    process_idx,
                    flow_id,
                    direction,
                    amount,
                });
                local_flow_ids.insert(flow_id);
            }

            Ok((
                process_meta,
                local_exchanges,
                local_flow_ids,
                local_reference,
                local_allocation,
            ))
        })
        .collect::<Vec<_>>();

    let mut exchanges = Vec::<ParsedExchange>::new();
    let mut process_meta_by_idx = HashMap::<i32, ProcessMeta>::with_capacity(processes.len());
    let mut flow_candidates: BTreeSet<Uuid> = BTreeSet::new();
    let mut reference_stats = ReferenceParseStats::default();
    let mut allocation_stats = AllocationParseStats::default();
    for chunk in chunks {
        let (meta, chunk_exchanges, chunk_flow_ids, chunk_reference, chunk_allocation) = chunk?;
        process_meta_by_idx.insert(meta.process_idx, meta);
        exchanges.extend(chunk_exchanges);
        flow_candidates.extend(chunk_flow_ids);
        reference_stats.missing_reference += chunk_reference.missing_reference;
        reference_stats.invalid_reference += chunk_reference.invalid_reference;
        reference_stats.normalized_processes += chunk_reference.normalized_processes;
        allocation_stats.exchange_total += chunk_allocation.exchange_total;
        allocation_stats.fraction_present_count += chunk_allocation.fraction_present_count;
        allocation_stats.fraction_missing_count += chunk_allocation.fraction_missing_count;
        allocation_stats.fraction_invalid_count += chunk_allocation.fraction_invalid_count;
    }

    let mut process_meta = Vec::with_capacity(processes.len());
    for idx in 0..process_count_i32 {
        process_meta.push(
            process_meta_by_idx
                .remove(&idx)
                .ok_or_else(|| anyhow::anyhow!("missing process meta for idx={idx}"))?,
        );
    }

    for impact in impact_factor_sets {
        for flow_id in impact.factors_by_flow.keys() {
            flow_candidates.insert(*flow_id);
        }
    }

    let flow_meta = fetch_flow_meta(pool, &flow_candidates).await?;
    let flow_ids = flow_candidates.into_iter().collect::<Vec<_>>();
    let flow_count = i32::try_from(flow_ids.len()).map_err(|_| anyhow::anyhow!("flow overflow"))?;
    let mut flow_idx_by_id = HashMap::with_capacity(flow_ids.len());
    let mut elementary_flow_idx = HashSet::new();
    for (idx, flow_id) in flow_ids.iter().enumerate() {
        let idx_i32 = i32::try_from(idx).map_err(|_| anyhow::anyhow!("flow idx overflow"))?;
        flow_idx_by_id.insert(*flow_id, idx_i32);
        let kind = flow_meta
            .get(flow_id)
            .map_or("product", |meta| classify_flow_kind(meta));
        if kind == "elementary" {
            elementary_flow_idx.insert(idx_i32);
        }
    }

    let mut provider_sets: HashMap<Uuid, HashSet<i32>> = HashMap::new();
    for ex in &exchanges {
        if ex.direction == ExchangeDirection::Output {
            provider_sets
                .entry(ex.flow_id)
                .or_default()
                .insert(ex.process_idx);
        }
    }
    let mut provider_map: HashMap<Uuid, Vec<i32>> = HashMap::with_capacity(provider_sets.len());
    for (flow_id, providers) in provider_sets {
        let mut sorted = providers.into_iter().collect::<Vec<_>>();
        sorted.sort_by_key(|idx| {
            process_meta_for_idx(&process_meta, *idx).map_or(Uuid::nil(), |meta| meta.process_id)
        });
        provider_map.insert(flow_id, sorted);
    }

    let mut a_map: HashMap<(i32, i32), f64> = HashMap::new();
    let mut b_map: HashMap<(i32, i32), f64> = HashMap::new();
    let mut input_edges_total: i64 = 0;
    let mut matched_unique: i64 = 0;
    let mut matched_multi: i64 = 0;
    let mut matched_multi_resolved: i64 = 0;
    let mut matched_multi_unresolved: i64 = 0;
    let mut matched_multi_fallback_equal: i64 = 0;
    let mut a_input_edges_written: i64 = 0;
    let mut unmatched: i64 = 0;

    for ex in &exchanges {
        if let Some(flow_idx) = flow_idx_by_id.get(&ex.flow_id).copied() {
            if elementary_flow_idx.contains(&flow_idx)
                && let Some(amount) = ex.amount
            {
                let value = biosphere_gross_value(amount);
                if value.abs() > f64::EPSILON {
                    *b_map.entry((flow_idx, ex.process_idx)).or_insert(0.0) += value;
                }
            }
        }

        if ex.direction != ExchangeDirection::Input {
            continue;
        }

        let Some(amount) = ex.amount else {
            continue;
        };
        input_edges_total += 1;
        let providers = provider_map.get(&ex.flow_id);
        let provider_cnt = providers.map_or(0, Vec::len);
        if provider_cnt == 1 {
            matched_unique += 1;
            a_input_edges_written += 1;
            let provider_idx = *providers
                .and_then(|vec| vec.first())
                .ok_or_else(|| anyhow::anyhow!("missing provider idx"))?;
            // A is stored as provider(row) -> consumer(col), so M = I - A
            // propagates demand from downstream to upstream with x = (I - A)^-1 y.
            add_technosphere_edge(&mut a_map, provider_idx, ex.process_idx, amount);
        } else if provider_cnt > 1 {
            matched_multi += 1;
            let resolution = resolve_multi_provider(
                provider_rule,
                ex,
                providers.ok_or_else(|| anyhow::anyhow!("missing provider list"))?,
                &process_meta,
            )?;
            if let Some(resolution) = resolution {
                matched_multi_resolved += 1;
                if resolution.used_equal_fallback {
                    matched_multi_fallback_equal += 1;
                }
                a_input_edges_written += 1;
                for (provider_idx, weight) in resolution.allocations {
                    let weighted = amount * weight;
                    add_technosphere_edge(&mut a_map, provider_idx, ex.process_idx, weighted);
                }
            } else {
                matched_multi_unresolved += 1;
            }
        } else {
            unmatched += 1;
        }
    }

    a_map.retain(|_, value| value.abs() > f64::EPSILON);
    b_map.retain(|_, value| value.abs() > f64::EPSILON);

    let prefilter_diag_ge_cutoff = i64::try_from(
        a_map
            .iter()
            .filter(|((row, col), value)| row == col && value.abs() >= self_loop_cutoff)
            .count(),
    )
    .map_err(|_| anyhow::anyhow!("prefilter count overflow"))?;

    let mut technosphere_entries = Vec::new();
    technosphere_entries.reserve(a_map.len());
    for ((row, col), value) in a_map {
        if row == col && value.abs() >= self_loop_cutoff {
            continue;
        }
        technosphere_entries.push(SparseTriplet { row, col, value });
    }

    let mut diag_a = HashMap::<i32, f64>::new();
    for t in &technosphere_entries {
        if t.row == t.col {
            *diag_a.entry(t.row).or_insert(0.0) += t.value;
        }
    }

    let a_diag_ge_cutoff = i64::try_from(
        diag_a
            .values()
            .filter(|value| value.abs() >= self_loop_cutoff)
            .count(),
    )
    .map_err(|_| anyhow::anyhow!("diag count overflow"))?;

    let mut m_zero_diag_count: i64 = 0;
    let mut m_min_abs_diag = f64::INFINITY;
    for idx in 0..process_count_i32 {
        let a_diag = diag_a.get(&idx).copied().unwrap_or(0.0);
        let abs_m_diag = (1.0 - a_diag).abs();
        if abs_m_diag <= singular_eps {
            m_zero_diag_count += 1;
        }
        if abs_m_diag < m_min_abs_diag {
            m_min_abs_diag = abs_m_diag;
        }
    }
    if !m_min_abs_diag.is_finite() {
        m_min_abs_diag = 0.0;
    }

    let risk_level = if m_zero_diag_count > 0 {
        "high".to_owned()
    } else if prefilter_diag_ge_cutoff > 0 || a_diag_ge_cutoff > 0 {
        "medium".to_owned()
    } else {
        "low".to_owned()
    };

    let mut biosphere_entries = Vec::with_capacity(b_map.len());
    for ((row, col), value) in b_map {
        biosphere_entries.push(SparseTriplet { row, col, value });
    }

    let mut characterization_factors = Vec::new();
    if has_lcia {
        for (impact_idx, impact) in impact_factor_sets.iter().enumerate() {
            let impact_row =
                i32::try_from(impact_idx).map_err(|_| anyhow::anyhow!("impact idx overflow"))?;
            let mut c_map = HashMap::<i32, f64>::new();
            for (flow_id, cf_value) in &impact.factors_by_flow {
                if let Some(flow_idx) = flow_idx_by_id.get(flow_id).copied()
                    && cf_value.abs() > f64::EPSILON
                {
                    *c_map.entry(flow_idx).or_insert(0.0) += *cf_value;
                }
            }
            c_map.retain(|_, value| value.abs() > f64::EPSILON);
            characterization_factors.reserve(c_map.len());
            for (col, value) in c_map {
                characterization_factors.push(SparseTriplet {
                    row: impact_row,
                    col,
                    value,
                });
            }
        }
    }

    let impact_count = if has_lcia {
        i32::try_from(impact_factor_sets.len()).map_err(|_| anyhow::anyhow!("impact overflow"))?
    } else {
        1_i32
    };
    let a_nnz = i64::try_from(technosphere_entries.len()).map_err(|_| anyhow::anyhow!("a nnz"))?;
    let b_nnz = i64::try_from(biosphere_entries.len()).map_err(|_| anyhow::anyhow!("b nnz"))?;
    let c_nnz =
        i64::try_from(characterization_factors.len()).map_err(|_| anyhow::anyhow!("c nnz"))?;
    let a_offdiag_nnz = i64::try_from(
        technosphere_entries
            .iter()
            .filter(|entry| entry.row != entry.col)
            .count(),
    )
    .map_err(|_| anyhow::anyhow!("a offdiag overflow"))?;
    let process_count_i64 = i64::from(process_count_i32);
    let m_nnz_estimated = a_offdiag_nnz + (process_count_i64 - m_zero_diag_count).max(0);
    let m_sparsity_estimated = if process_count_i64 == 0 {
        1.0
    } else {
        1.0 - (m_nnz_estimated as f64 / (process_count_i64 * process_count_i64) as f64)
    };

    let unique_provider_match_pct = pct(matched_unique, input_edges_total);
    let any_provider_match_pct = pct(matched_unique + matched_multi, input_edges_total);
    let allocation_fraction_present_pct = pct(
        allocation_stats.fraction_present_count,
        allocation_stats.exchange_total,
    );

    let coverage = SnapshotCoverageReport {
        matching: SnapshotMatchingCoverage {
            input_edges_total,
            matched_unique_provider: matched_unique,
            matched_multi_provider: matched_multi,
            unmatched_no_provider: unmatched,
            matched_multi_resolved,
            matched_multi_unresolved,
            matched_multi_fallback_equal,
            a_input_edges_written,
            unique_provider_match_pct,
            any_provider_match_pct,
        },
        reference: SnapshotReferenceCoverage {
            process_total: process_count_i64,
            normalized_process_count: reference_stats.normalized_processes,
            missing_reference_count: reference_stats.missing_reference,
            invalid_reference_count: reference_stats.invalid_reference,
        },
        allocation: SnapshotAllocationCoverage {
            exchange_total: allocation_stats.exchange_total,
            allocation_fraction_present_pct,
            allocation_fraction_missing_count: allocation_stats.fraction_missing_count,
            allocation_fraction_invalid_count: allocation_stats.fraction_invalid_count,
        },
        singular_risk: SnapshotSingularRisk {
            risk_level,
            prefilter_diag_abs_ge_cutoff: prefilter_diag_ge_cutoff,
            postfilter_a_diag_abs_ge_cutoff: a_diag_ge_cutoff,
            m_zero_diagonal_count: m_zero_diag_count,
            m_min_abs_diagonal: m_min_abs_diag,
        },
        matrix_scale: SnapshotMatrixScale {
            process_count: process_count_i64,
            flow_count: i64::from(flow_count),
            impact_count: i64::from(impact_count),
            a_nnz,
            b_nnz,
            c_nnz,
            m_nnz_estimated,
            m_sparsity_estimated,
        },
    };

    let data = ModelSparseData {
        model_version: snapshot_id,
        process_count: process_count_i32,
        flow_count,
        impact_count,
        technosphere_entries,
        biosphere_entries,
        characterization_factors,
    };
    let process_map = process_meta
        .iter()
        .map(|meta| SnapshotProcessMapEntry {
            process_id: meta.process_id,
            process_index: meta.process_idx,
            process_version: meta.process_version.clone(),
            process_name: meta.process_name.clone(),
            location: meta.location.clone(),
        })
        .collect::<Vec<_>>();
    let impact_map = build_snapshot_impact_map(snapshot_id, method, impact_factor_sets)?;

    let snapshot_index = SnapshotIndexDocument {
        version: 1,
        snapshot_id,
        process_count: process_count_i32,
        impact_count,
        process_map,
        impact_map,
    };

    Ok(BuildOutput {
        data,
        coverage,
        snapshot_index,
    })
}

fn build_snapshot_impact_map(
    snapshot_id: Uuid,
    method: &MethodSelection,
    impact_factor_sets: &[ImpactFactorSet],
) -> anyhow::Result<Vec<SnapshotImpactMapEntry>> {
    if !method.has_lcia {
        return Ok(vec![SnapshotImpactMapEntry {
            impact_id: snapshot_id,
            impact_index: 0,
            impact_key: "lcia-disabled".to_owned(),
            impact_name: "LCIA disabled (placeholder impact)".to_owned(),
            unit: "unknown".to_owned(),
        }]);
    }
    if impact_factor_sets.is_empty() {
        return Err(anyhow::anyhow!(
            "LCIA is enabled but no impact factors were loaded"
        ));
    }

    let mut out = Vec::with_capacity(impact_factor_sets.len());
    for (impact_idx, impact) in impact_factor_sets.iter().enumerate() {
        out.push(SnapshotImpactMapEntry {
            impact_id: impact.impact_id,
            impact_index: i32::try_from(impact_idx)
                .map_err(|_| anyhow::anyhow!("impact index overflow"))?,
            impact_key: impact.impact_key.clone(),
            impact_name: impact.impact_name.clone(),
            unit: impact.unit.clone(),
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct MultiProviderResolution {
    allocations: Vec<(i32, f64)>,
    used_equal_fallback: bool,
}

const AUTO_LINK_GEO_WEIGHT: f64 = 0.7;
const AUTO_LINK_TIME_WEIGHT: f64 = 0.3;
const AUTO_LINK_MIN_SCORE: f64 = 0.35;
const AUTO_LINK_TOP1_MIN_SCORE: f64 = 0.55;
const AUTO_LINK_TOP1_TOP2_MIN_RATIO: f64 = 1.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AllocationFractionState {
    Present,
    Missing,
    Invalid,
}

fn resolve_reference_normalization(
    process_id: Uuid,
    process_json: &Value,
    exchanges: &[&Value],
    mode: NormalizationMode,
) -> anyhow::Result<(f64, ReferenceParseStats)> {
    let mut stats = ReferenceParseStats::default();
    let reference_internal_id = process_json
        .get("processDataSet")
        .and_then(|v| v.get("processInformation"))
        .and_then(|v| v.get("quantitativeReference"))
        .and_then(|v| v.get("referenceToReferenceFlow"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(reference_internal_id) = reference_internal_id else {
        stats.missing_reference = 1;
        return match mode {
            NormalizationMode::Strict => Err(anyhow::anyhow!(
                "missing quantitativeReference.referenceToReferenceFlow for process={process_id}"
            )),
            NormalizationMode::Lenient => Ok((1.0, stats)),
        };
    };

    let reference_exchange = exchanges.iter().copied().find(|exchange| {
        exchange
            .get("@dataSetInternalID")
            .and_then(Value::as_str)
            .map(str::trim)
            == Some(reference_internal_id)
    });

    let Some(reference_exchange) = reference_exchange else {
        stats.invalid_reference = 1;
        return match mode {
            NormalizationMode::Strict => Err(anyhow::anyhow!(
                "referenceToReferenceFlow={} not found in exchanges for process={process_id}",
                reference_internal_id
            )),
            NormalizationMode::Lenient => Ok((1.0, stats)),
        };
    };

    let reference_amount = parse_number(
        reference_exchange
            .get("meanAmount")
            .or_else(|| reference_exchange.get("resultingAmount"))
            .or_else(|| reference_exchange.get("meanValue")),
    )
    .map(f64::abs)
    .filter(|value| *value > f64::EPSILON);
    let Some(reference_amount) = reference_amount else {
        stats.invalid_reference = 1;
        return match mode {
            NormalizationMode::Strict => Err(anyhow::anyhow!(
                "invalid reference amount for process={process_id} reference_internal_id={}",
                reference_internal_id
            )),
            NormalizationMode::Lenient => Ok((1.0, stats)),
        };
    };

    stats.normalized_processes = 1;
    Ok((1.0 / reference_amount, stats))
}

fn resolve_allocation_fraction(
    exchange_json: &Value,
    mode: AllocationMode,
) -> anyhow::Result<(f64, AllocationFractionState)> {
    let raw = exchange_json
        .get("allocations")
        .and_then(|v| v.get("allocation"))
        .and_then(|v| v.get("@allocatedFraction"));
    let Some(raw) = raw else {
        return match mode {
            AllocationMode::Strict => Err(anyhow::anyhow!(
                "missing allocations.allocation.@allocatedFraction"
            )),
            AllocationMode::Lenient => Ok((1.0, AllocationFractionState::Missing)),
        };
    };

    let parsed = match raw {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else if let Some(without_percent) = trimmed.strip_suffix('%') {
                without_percent
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(|value| value / 100.0)
            } else {
                trimmed.parse::<f64>().ok().map(
                    |value| {
                        if value > 1.0 { value / 100.0 } else { value }
                    },
                )
            }
        }
        _ => None,
    };
    let fraction = parsed.filter(|value| value.is_finite() && *value >= 0.0 && *value <= 1.0);
    let Some(fraction) = fraction else {
        return match mode {
            AllocationMode::Strict => Err(anyhow::anyhow!(
                "invalid allocations.allocation.@allocatedFraction={}",
                raw
            )),
            AllocationMode::Lenient => Ok((1.0, AllocationFractionState::Invalid)),
        };
    };

    Ok((fraction, AllocationFractionState::Present))
}

fn resolve_multi_provider(
    provider_rule: ProviderRule,
    exchange: &ParsedExchange,
    providers: &[i32],
    process_meta: &[ProcessMeta],
) -> anyhow::Result<Option<MultiProviderResolution>> {
    if providers.is_empty() {
        return Ok(None);
    }

    let split_equal = || -> MultiProviderResolution {
        let share = 1.0 / providers.len() as f64;
        MultiProviderResolution {
            allocations: providers.iter().map(|idx| (*idx, share)).collect(),
            used_equal_fallback: true,
        }
    };

    let scored_candidates = |min_score: f64| -> anyhow::Result<Vec<ProviderCandidateScore>> {
        let mut scored = score_provider_candidates(exchange.process_idx, providers, process_meta)?;
        scored.retain(|candidate| candidate.final_score >= min_score);
        Ok(scored)
    };

    let debug_label = format!(
        "flow={} consumer_idx={}",
        exchange.flow_id, exchange.process_idx
    );

    match provider_rule {
        ProviderRule::StrictUniqueProvider => Ok(None),
        ProviderRule::SplitEqual => Ok(Some(split_equal())),
        ProviderRule::BestProviderStrict => {
            let scored = scored_candidates(AUTO_LINK_MIN_SCORE)?;
            let top1 = scored.first().ok_or_else(|| {
                anyhow::anyhow!(
                    "best_provider_strict failed (no candidate >= min_score): {debug_label}"
                )
            })?;
            if top1.final_score < AUTO_LINK_TOP1_MIN_SCORE {
                return Err(anyhow::anyhow!(
                    "best_provider_strict failed (top1 score < {}): {}",
                    AUTO_LINK_TOP1_MIN_SCORE,
                    debug_label
                ));
            }
            if let Some(top2) = scored.get(1)
                && top2.final_score > f64::EPSILON
                && (top1.final_score / top2.final_score) < AUTO_LINK_TOP1_TOP2_MIN_RATIO
            {
                return Err(anyhow::anyhow!(
                    "best_provider_strict failed (top1/top2 ratio < {}): {}",
                    AUTO_LINK_TOP1_TOP2_MIN_RATIO,
                    debug_label
                ));
            }

            Ok(Some(MultiProviderResolution {
                allocations: vec![(top1.provider_idx, 1.0)],
                used_equal_fallback: false,
            }))
        }
        ProviderRule::SplitByEvidenceStrict => {
            let scored = scored_candidates(AUTO_LINK_MIN_SCORE)?;
            if scored.is_empty() {
                return Err(anyhow::anyhow!(
                    "split_by_evidence failed (no candidate >= min_score): {debug_label}"
                ));
            }
            let score_sum = scored
                .iter()
                .map(|candidate| candidate.final_score)
                .sum::<f64>();
            if score_sum <= f64::EPSILON {
                return Err(anyhow::anyhow!(
                    "split_by_evidence failed (score sum <= 0): {debug_label}"
                ));
            }
            Ok(Some(MultiProviderResolution {
                allocations: scored
                    .iter()
                    .map(|candidate| (candidate.provider_idx, candidate.final_score / score_sum))
                    .collect(),
                used_equal_fallback: false,
            }))
        }
        ProviderRule::SplitByEvidenceHybrid => {
            let scored = scored_candidates(AUTO_LINK_MIN_SCORE)?;
            if scored.is_empty() {
                return Ok(Some(split_equal()));
            }
            let score_sum = scored
                .iter()
                .map(|candidate| candidate.final_score)
                .sum::<f64>();
            if score_sum <= f64::EPSILON {
                return Ok(Some(split_equal()));
            }
            Ok(Some(MultiProviderResolution {
                allocations: scored
                    .iter()
                    .map(|candidate| (candidate.provider_idx, candidate.final_score / score_sum))
                    .collect(),
                used_equal_fallback: false,
            }))
        }
    }
}

fn score_provider_candidates(
    consumer_idx: i32,
    providers: &[i32],
    process_meta: &[ProcessMeta],
) -> anyhow::Result<Vec<ProviderCandidateScore>> {
    let consumer = process_meta_for_idx(process_meta, consumer_idx)
        .ok_or_else(|| anyhow::anyhow!("missing consumer process meta idx={consumer_idx}"))?;
    let mut candidate_indices = Vec::<i32>::new();
    if let Some(consumer_model_id) = consumer.model_id {
        for provider_idx in providers {
            if process_meta_for_idx(process_meta, *provider_idx)
                .is_some_and(|provider| provider.model_id == Some(consumer_model_id))
            {
                candidate_indices.push(*provider_idx);
            }
        }
    }
    if candidate_indices.is_empty() {
        candidate_indices.extend_from_slice(providers);
    }

    let mut scored = Vec::with_capacity(providers.len());
    for provider_idx in &candidate_indices {
        let provider = process_meta_for_idx(process_meta, *provider_idx)
            .ok_or_else(|| anyhow::anyhow!("missing provider process meta idx={provider_idx}"))?;
        let geo = geo_score(consumer.location.as_deref(), provider.location.as_deref());
        let time = time_score(consumer.reference_year, provider.reference_year);
        let final_score = AUTO_LINK_GEO_WEIGHT * geo + AUTO_LINK_TIME_WEIGHT * time;
        scored.push(ProviderCandidateScore {
            provider_idx: *provider_idx,
            provider_id: provider.process_id,
            geo_score: geo,
            time_score: time,
            final_score,
        });
    }

    scored.sort_by(|left, right| {
        right
            .final_score
            .total_cmp(&left.final_score)
            .then_with(|| right.geo_score.total_cmp(&left.geo_score))
            .then_with(|| right.time_score.total_cmp(&left.time_score))
            .then_with(|| left.provider_id.cmp(&right.provider_id))
    });
    Ok(scored)
}

fn process_meta_for_idx(process_meta: &[ProcessMeta], process_idx: i32) -> Option<&ProcessMeta> {
    usize::try_from(process_idx)
        .ok()
        .and_then(|idx| process_meta.get(idx))
}

#[derive(Debug, Clone)]
struct LocationDescriptor {
    canonical: Option<String>,
    country_code: Option<String>,
    region_group: Option<&'static str>,
    is_subnational: bool,
    is_global: bool,
}

fn geo_score(consumer_location: Option<&str>, provider_location: Option<&str>) -> f64 {
    let consumer = parse_location_descriptor(consumer_location);
    let provider = parse_location_descriptor(provider_location);
    if consumer.is_subnational
        && provider.is_subnational
        && consumer.canonical.is_some()
        && consumer.canonical == provider.canonical
    {
        return 1.0;
    }
    if consumer.country_code.is_some() && consumer.country_code == provider.country_code {
        return 0.85;
    }
    if consumer.region_group.is_some() && consumer.region_group == provider.region_group {
        return 0.6;
    }
    if provider.is_global {
        return 0.4;
    }
    0.1
}

fn time_score(consumer_year: Option<i32>, provider_year: Option<i32>) -> f64 {
    match (consumer_year, provider_year) {
        (Some(left), Some(right)) => {
            let diff = (left - right).abs();
            if diff <= 1 {
                1.0
            } else if diff <= 3 {
                0.85
            } else if diff <= 5 {
                0.65
            } else if diff <= 10 {
                0.4
            } else {
                0.2
            }
        }
        _ => 0.5,
    }
}

fn parse_location_descriptor(location: Option<&str>) -> LocationDescriptor {
    let canonical = location
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase().replace('_', "-"));
    let Some(canonical) = canonical else {
        return LocationDescriptor {
            canonical: None,
            country_code: None,
            region_group: None,
            is_subnational: false,
            is_global: false,
        };
    };

    let is_global = canonical == "GLO";
    let country_code = extract_country_code(&canonical);
    let is_subnational = canonical.contains('-') && country_code.is_some();
    let region_group = region_group_from_code(&canonical)
        .or_else(|| country_code.as_deref().and_then(region_group_from_code));
    LocationDescriptor {
        canonical: Some(canonical),
        country_code,
        region_group,
        is_subnational,
        is_global,
    }
}

fn extract_country_code(location: &str) -> Option<String> {
    if location == "GLO" {
        return None;
    }
    let token = location
        .split(['-', '_', ' '])
        .next()
        .map(str::trim)
        .unwrap_or_default();
    if token.len() == 2 && token.chars().all(|chr| chr.is_ascii_alphabetic()) {
        Some(token.to_owned())
    } else {
        None
    }
}

fn region_group_from_code(code: &str) -> Option<&'static str> {
    match code {
        "GLO" => Some("GLOBAL"),
        "RER" | "EU" | "EU27" | "EU28" | "EFTA" | "WEU" | "EEU" | "AT" | "BE" | "BG" | "CH"
        | "CY" | "CZ" | "DE" | "DK" | "EE" | "ES" | "FI" | "FR" | "GB" | "GR" | "HR" | "HU"
        | "IE" | "IS" | "IT" | "LI" | "LT" | "LU" | "LV" | "MT" | "NL" | "NO" | "PL" | "PT"
        | "RO" | "SE" | "SI" | "SK" => Some("EUROPE"),
        "APAC" | "RAS" | "SAS" | "EAS" | "OCE" | "CN" | "JP" | "KR" | "IN" | "AU" | "NZ" | "ID"
        | "TH" | "VN" | "MY" | "SG" | "PH" | "PK" | "BD" => Some("APAC"),
        "RNA" | "NAM" | "US" | "CA" | "MX" => Some("NORTH_AMERICA"),
        "RLA" | "LATAM" | "BR" | "AR" | "CL" | "CO" | "PE" | "UY" | "PY" | "BO" | "EC" | "VE"
        | "CR" | "GT" | "HN" | "NI" | "SV" | "PA" | "DO" | "CU" => Some("LATAM"),
        "RAF" | "AFR" | "ZA" | "EG" | "NG" | "KE" | "GH" | "DZ" | "MA" | "TN" | "ET" | "TZ"
        | "UG" => Some("AFRICA"),
        "RME" | "MEA" | "AE" | "SA" | "QA" | "KW" | "OM" | "BH" | "IL" | "TR" | "IR" | "IQ"
        | "JO" | "LB" => Some("MIDDLE_EAST"),
        _ => None,
    }
}

fn pct(numerator: i64, denominator: i64) -> f64 {
    if denominator <= 0 {
        0.0
    } else {
        ((numerator as f64 / denominator as f64) * 10000.0).round() / 100.0
    }
}

fn biosphere_gross_value(amount: f64) -> f64 {
    amount
}

async fn compute_source_fingerprint(
    pool: &PgPool,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
    process_limit: usize,
    config: &SnapshotBuildConfig,
) -> anyhow::Result<(SourceSnapshotSummary, String)> {
    let (process_count, process_max_modified_at_utc) = fetch_process_source_summary(
        pool,
        all_states,
        state_codes,
        include_user_id,
        process_limit,
    )
    .await?;
    let (flow_count, flow_max_modified_at_utc) = fetch_flow_source_summary(pool).await?;
    let (lciamethod_count, lciamethod_max_modified_at_utc) =
        fetch_lciamethod_source_summary(pool).await?;

    let summary = SourceSnapshotSummary {
        process_count,
        process_max_modified_at_utc,
        flow_count,
        flow_max_modified_at_utc,
        lciamethod_count,
        lciamethod_max_modified_at_utc,
    };

    let body = serde_json::json!({
        "schema": "source-fingerprint:v1",
        "source": {
            "processes": {
                "count": summary.process_count,
                "max_modified_at_utc": summary.process_max_modified_at_utc,
            },
            "flows": {
                "count": summary.flow_count,
                "max_modified_at_utc": summary.flow_max_modified_at_utc,
            },
            "lciamethods": {
                "count": summary.lciamethod_count,
                "max_modified_at_utc": summary.lciamethod_max_modified_at_utc,
            }
        },
        "config": config,
    });

    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(&body)?);
    let fingerprint = hex::encode(hasher.finalize());
    Ok((summary, fingerprint))
}

async fn fetch_process_source_summary(
    pool: &PgPool,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
    process_limit: usize,
) -> anyhow::Result<(i64, String)> {
    let limit =
        i64::try_from(process_limit).map_err(|_| anyhow::anyhow!("process_limit overflow"))?;
    let row = if all_states {
        sqlx::query(
            r#"
            WITH candidate AS (
              SELECT modified_at
              FROM public.processes
              WHERE json ? 'processDataSet'
              ORDER BY id, version
              LIMIT NULLIF($1::bigint, 0)
            )
            SELECT
              COUNT(*)::bigint AS process_count,
              COALESCE(
                to_char(MAX(modified_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
                'none'
              ) AS process_max_modified_at_utc
            FROM candidate
            "#,
        )
        .bind(limit)
        .fetch_one(pool)
        .await?
    } else if let Some(user_id) = include_user_id {
        sqlx::query(
            r#"
            WITH candidate AS (
              SELECT modified_at
              FROM public.processes
              WHERE (state_code = ANY($1) OR user_id = $2)
                AND json ? 'processDataSet'
              ORDER BY id, version
              LIMIT NULLIF($3::bigint, 0)
            )
            SELECT
              COUNT(*)::bigint AS process_count,
              COALESCE(
                to_char(MAX(modified_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
                'none'
              ) AS process_max_modified_at_utc
            FROM candidate
            "#,
        )
        .bind(state_codes)
        .bind(user_id)
        .bind(limit)
        .fetch_one(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            WITH candidate AS (
              SELECT modified_at
              FROM public.processes
              WHERE state_code = ANY($1)
                AND json ? 'processDataSet'
              ORDER BY id, version
              LIMIT NULLIF($2::bigint, 0)
            )
            SELECT
              COUNT(*)::bigint AS process_count,
              COALESCE(
                to_char(MAX(modified_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
                'none'
              ) AS process_max_modified_at_utc
            FROM candidate
            "#,
        )
        .bind(state_codes)
        .bind(limit)
        .fetch_one(pool)
        .await?
    };

    Ok((
        row.try_get::<i64, _>("process_count")?,
        row.try_get::<String, _>("process_max_modified_at_utc")?,
    ))
}

async fn fetch_flow_source_summary(pool: &PgPool) -> anyhow::Result<(i64, String)> {
    let row = sqlx::query(
        r#"
        SELECT
          COUNT(*)::bigint AS flow_count,
          COALESCE(
            to_char(MAX(modified_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'none'
          ) AS flow_max_modified_at_utc
        FROM public.flows
        "#,
    )
    .fetch_one(pool)
    .await?;

    Ok((
        row.try_get::<i64, _>("flow_count")?,
        row.try_get::<String, _>("flow_max_modified_at_utc")?,
    ))
}

async fn fetch_lciamethod_source_summary(pool: &PgPool) -> anyhow::Result<(i64, String)> {
    let row = sqlx::query(
        r#"
        SELECT
          COUNT(*)::bigint AS lciamethod_count,
          COALESCE(
            to_char(MAX(modified_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'),
            'none'
          ) AS lciamethod_max_modified_at_utc
        FROM public.lciamethods
        "#,
    )
    .fetch_one(pool)
    .await?;

    Ok((
        row.try_get::<i64, _>("lciamethod_count")?,
        row.try_get::<String, _>("lciamethod_max_modified_at_utc")?,
    ))
}

async fn find_reusable_snapshot(
    pool: &PgPool,
    source_fingerprint: &str,
) -> anyhow::Result<Option<ReuseCandidate>> {
    let row = sqlx::query(
        r#"
        SELECT
          s.id AS snapshot_id,
          a.artifact_url,
          a.artifact_sha256,
          a.artifact_byte_size,
          a.artifact_format,
          a.coverage,
          a.process_count::bigint AS process_count,
          a.flow_count::bigint AS flow_count,
          a.impact_count::bigint AS impact_count,
          a.a_nnz,
          a.b_nnz,
          a.c_nnz
        FROM public.lca_network_snapshots s
        INNER JOIN public.lca_snapshot_artifacts a
          ON a.snapshot_id = s.id
        WHERE s.status = 'ready'
          AND a.status = 'ready'
          AND a.artifact_format = $2
          AND s.source_hash = $1
        ORDER BY a.created_at DESC
        LIMIT 1
        "#,
    )
    .bind(source_fingerprint)
    .bind(SNAPSHOT_ARTIFACT_FORMAT)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let coverage_value = row.try_get::<Option<Value>, _>("coverage")?;
    let Some(coverage_value) = coverage_value else {
        return Ok(None);
    };
    let coverage: SnapshotCoverageReport = serde_json::from_value(coverage_value)?;

    Ok(Some(ReuseCandidate {
        snapshot_id: row.try_get::<Uuid, _>("snapshot_id")?,
        artifact_url: row.try_get::<String, _>("artifact_url")?,
        artifact_sha256: row.try_get::<String, _>("artifact_sha256")?,
        artifact_byte_size: row.try_get::<i64, _>("artifact_byte_size")?,
        artifact_format: row.try_get::<String, _>("artifact_format")?,
        coverage,
        process_count: row.try_get::<i64, _>("process_count")?,
        flow_count: row.try_get::<i64, _>("flow_count")?,
        impact_count: row.try_get::<i64, _>("impact_count")?,
        a_nnz: row.try_get::<i64, _>("a_nnz")?,
        b_nnz: row.try_get::<i64, _>("b_nnz")?,
        c_nnz: row.try_get::<i64, _>("c_nnz")?,
    }))
}

async fn fetch_processes(
    pool: &PgPool,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
) -> anyhow::Result<Vec<ProcessRow>> {
    let rows = if all_states {
        sqlx::query(
            r#"
            SELECT id, version, model_id, json
            FROM public.processes
            WHERE json ? 'processDataSet'
            ORDER BY id, version
            "#,
        )
        .fetch_all(pool)
        .await?
    } else if let Some(user_id) = include_user_id {
        sqlx::query(
            r#"
            SELECT id, version, model_id, json
            FROM public.processes
            WHERE (state_code = ANY($1) OR user_id = $2)
              AND json ? 'processDataSet'
            ORDER BY id, version
            "#,
        )
        .bind(state_codes)
        .bind(user_id)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT id, version, model_id, json
            FROM public.processes
            WHERE state_code = ANY($1)
              AND json ? 'processDataSet'
            ORDER BY id, version
            "#,
        )
        .bind(state_codes)
        .fetch_all(pool)
        .await?
    };

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(ProcessRow {
            id: row.try_get::<Uuid, _>("id")?,
            version: row.try_get::<String, _>("version")?.trim().to_owned(),
            model_id: row.try_get::<Option<Uuid>, _>("model_id")?,
            json: row.try_get::<Value, _>("json")?,
        });
    }
    Ok(out)
}

async fn fetch_flow_meta(
    pool: &PgPool,
    flow_candidates: &BTreeSet<Uuid>,
) -> anyhow::Result<HashMap<Uuid, Value>> {
    if flow_candidates.is_empty() {
        return Ok(HashMap::new());
    }
    let candidate_ids = flow_candidates.iter().copied().collect::<Vec<_>>();
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT ON (id) id, json
        FROM public.flows
        WHERE id = ANY($1)
        ORDER BY id, state_code DESC, modified_at DESC NULLS LAST, created_at DESC NULLS LAST
        "#,
    )
    .bind(&candidate_ids)
    .fetch_all(pool)
    .await?;

    let mut out = HashMap::<Uuid, Value>::new();
    for row in rows {
        let id = row.try_get::<Uuid, _>("id")?;
        out.insert(id, row.try_get::<Value, _>("json")?);
    }
    Ok(out)
}

fn process_exchange_items(process_json: &Value) -> Vec<&Value> {
    let Some(exchange) = process_json
        .get("processDataSet")
        .and_then(|v| v.get("exchanges"))
        .and_then(|v| v.get("exchange"))
    else {
        return Vec::new();
    };

    match exchange {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![exchange],
        _ => Vec::new(),
    }
}

fn method_factor_items(method_json: &Value) -> Vec<&Value> {
    let Some(factor) = method_json
        .get("LCIAMethodDataSet")
        .and_then(|v| v.get("characterisationFactors"))
        .and_then(|v| v.get("factor"))
    else {
        return Vec::new();
    };

    match factor {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![factor],
        _ => Vec::new(),
    }
}

fn parse_uuid_at(value: &Value, path: &[&str]) -> Option<Uuid> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().and_then(|s| Uuid::parse_str(s).ok())
}

fn parse_number(value: Option<&Value>) -> Option<f64> {
    match value {
        Some(Value::String(text)) => {
            let cleaned = text.replace(',', "");
            cleaned.parse::<f64>().ok()
        }
        Some(Value::Number(number)) => number.as_f64(),
        _ => None,
    }
}

fn parse_process_location(process_json: &Value) -> Option<String> {
    process_json
        .get("processDataSet")
        .and_then(|v| v.get("processInformation"))
        .and_then(|v| v.get("geography"))
        .and_then(|v| v.get("locationOfOperationSupplyOrProduction"))
        .and_then(|v| v.get("@location"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_process_name(process_json: &Value) -> Option<String> {
    process_json
        .get("processDataSet")
        .and_then(|v| v.get("processInformation"))
        .and_then(|v| v.get("dataSetInformation"))
        .and_then(|v| v.get("name"))
        .and_then(|v| v.get("baseName"))
        .and_then(|v| match v {
            Value::Array(items) => items.first(),
            Value::Object(_) => Some(v),
            _ => None,
        })
        .and_then(|v| v.get("#text").or(Some(v)))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_process_reference_year(process_json: &Value) -> Option<i32> {
    let value = process_json
        .get("processDataSet")
        .and_then(|v| v.get("processInformation"))
        .and_then(|v| v.get("time"))
        .and_then(|v| v.get("common:referenceYear"))?;
    match value {
        Value::Number(number) => number.as_i64().and_then(|year| i32::try_from(year).ok()),
        Value::String(text) => text.trim().parse::<i32>().ok(),
        _ => None,
    }
}

fn classify_flow_kind(flow_json: &Value) -> &'static str {
    let Some(category) = flow_json
        .get("flowDataSet")
        .and_then(|v| v.get("flowInformation"))
        .and_then(|v| v.get("dataSetInformation"))
        .and_then(|v| v.get("classificationInformation"))
        .and_then(|v| v.get("common:elementaryFlowCategorization"))
        .and_then(|v| v.get("common:category"))
    else {
        return "product";
    };

    let category_text = match category {
        Value::Array(arr) => arr
            .first()
            .and_then(|v| v.get("#text"))
            .and_then(Value::as_str)
            .unwrap_or_default(),
        Value::Object(obj) => obj.get("#text").and_then(Value::as_str).unwrap_or_default(),
        Value::String(text) => text.as_str(),
        _ => "",
    };

    match category_text {
        "Emissions" | "Resources" | "Land use" => "elementary",
        _ => "product",
    }
}

#[allow(clippy::too_many_arguments)]
async fn persist_snapshot_metadata(
    pool: &PgPool,
    snapshot_id: Uuid,
    provider_rule: &str,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
    source_hash: &str,
    method: &MethodSelection,
    built: &BuildOutput,
    artifact_url: &str,
    artifact_sha256: &str,
    artifact_byte_size: i64,
    artifact_format: &str,
) -> anyhow::Result<()> {
    let process_filter = if all_states {
        serde_json::json!({"all_states": true})
    } else if let Some(user_id) = include_user_id {
        serde_json::json!({
            "all_states": false,
            "process_states": state_codes,
            "include_user_id": user_id,
        })
    } else {
        serde_json::json!({"all_states": false, "process_states": state_codes})
    };

    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO public.lca_network_snapshots (
            id,
            scope,
            process_filter,
            lcia_method_id,
            lcia_method_version,
            provider_matching_rule,
            source_hash,
            status,
            created_at,
            updated_at
        )
        VALUES ($1, 'full_library', $2::jsonb, $3, $4::bpchar, $5, $6, 'ready', NOW(), NOW())
        ON CONFLICT (id)
        DO UPDATE SET
            process_filter = EXCLUDED.process_filter,
            lcia_method_id = EXCLUDED.lcia_method_id,
            lcia_method_version = EXCLUDED.lcia_method_version,
            provider_matching_rule = EXCLUDED.provider_matching_rule,
            source_hash = EXCLUDED.source_hash,
            status = EXCLUDED.status,
            updated_at = NOW()
        "#,
    )
    .bind(snapshot_id)
    .bind(process_filter)
    .bind(method.method_id)
    .bind(method.method_version.clone())
    .bind(provider_rule)
    .bind(source_hash)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO public.lca_snapshot_artifacts (
            snapshot_id,
            artifact_url,
            artifact_sha256,
            artifact_byte_size,
            artifact_format,
            process_count,
            flow_count,
            impact_count,
            a_nnz,
            b_nnz,
            c_nnz,
            coverage,
            status,
            created_at,
            updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10, $11,
            $12::jsonb, 'ready', NOW(), NOW()
        )
        ON CONFLICT (snapshot_id, artifact_format)
        DO UPDATE SET
            artifact_url = EXCLUDED.artifact_url,
            artifact_sha256 = EXCLUDED.artifact_sha256,
            artifact_byte_size = EXCLUDED.artifact_byte_size,
            process_count = EXCLUDED.process_count,
            flow_count = EXCLUDED.flow_count,
            impact_count = EXCLUDED.impact_count,
            a_nnz = EXCLUDED.a_nnz,
            b_nnz = EXCLUDED.b_nnz,
            c_nnz = EXCLUDED.c_nnz,
            coverage = EXCLUDED.coverage,
            status = EXCLUDED.status,
            updated_at = NOW()
        "#,
    )
    .bind(snapshot_id)
    .bind(artifact_url)
    .bind(artifact_sha256)
    .bind(artifact_byte_size)
    .bind(artifact_format)
    .bind(i64::from(built.data.process_count))
    .bind(i64::from(built.data.flow_count))
    .bind(i64::from(built.data.impact_count))
    .bind(built.coverage.matrix_scale.a_nnz)
    .bind(built.coverage.matrix_scale.b_nnz)
    .bind(built.coverage.matrix_scale.c_nnz)
    .bind(serde_json::to_value(&built.coverage)?)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

async fn persist_reused_snapshot_metadata(
    pool: &PgPool,
    snapshot_id: Uuid,
    provider_rule: &str,
    all_states: bool,
    state_codes: &[i32],
    include_user_id: Option<Uuid>,
    source_hash: &str,
    method: &MethodSelection,
    reused: &ReuseCandidate,
) -> anyhow::Result<()> {
    let process_filter = if all_states {
        serde_json::json!({"all_states": true})
    } else if let Some(user_id) = include_user_id {
        serde_json::json!({
            "all_states": false,
            "process_states": state_codes,
            "include_user_id": user_id,
        })
    } else {
        serde_json::json!({"all_states": false, "process_states": state_codes})
    };

    let mut tx = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO public.lca_network_snapshots (
            id,
            scope,
            process_filter,
            lcia_method_id,
            lcia_method_version,
            provider_matching_rule,
            source_hash,
            status,
            created_at,
            updated_at
        )
        VALUES ($1, 'full_library', $2::jsonb, $3, $4::bpchar, $5, $6, 'ready', NOW(), NOW())
        ON CONFLICT (id)
        DO UPDATE SET
            process_filter = EXCLUDED.process_filter,
            lcia_method_id = EXCLUDED.lcia_method_id,
            lcia_method_version = EXCLUDED.lcia_method_version,
            provider_matching_rule = EXCLUDED.provider_matching_rule,
            source_hash = EXCLUDED.source_hash,
            status = EXCLUDED.status,
            updated_at = NOW()
        "#,
    )
    .bind(snapshot_id)
    .bind(process_filter)
    .bind(method.method_id)
    .bind(method.method_version.clone())
    .bind(provider_rule)
    .bind(source_hash)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO public.lca_snapshot_artifacts (
            snapshot_id,
            artifact_url,
            artifact_sha256,
            artifact_byte_size,
            artifact_format,
            process_count,
            flow_count,
            impact_count,
            a_nnz,
            b_nnz,
            c_nnz,
            coverage,
            status,
            created_at,
            updated_at
        )
        VALUES (
            $1, $2, $3, $4, $5,
            $6, $7, $8, $9, $10, $11,
            $12::jsonb, 'ready', NOW(), NOW()
        )
        ON CONFLICT (snapshot_id, artifact_format)
        DO UPDATE SET
            artifact_url = EXCLUDED.artifact_url,
            artifact_sha256 = EXCLUDED.artifact_sha256,
            artifact_byte_size = EXCLUDED.artifact_byte_size,
            process_count = EXCLUDED.process_count,
            flow_count = EXCLUDED.flow_count,
            impact_count = EXCLUDED.impact_count,
            a_nnz = EXCLUDED.a_nnz,
            b_nnz = EXCLUDED.b_nnz,
            c_nnz = EXCLUDED.c_nnz,
            coverage = EXCLUDED.coverage,
            status = EXCLUDED.status,
            updated_at = NOW()
        "#,
    )
    .bind(snapshot_id)
    .bind(reused.artifact_url.as_str())
    .bind(reused.artifact_sha256.as_str())
    .bind(reused.artifact_byte_size)
    .bind(reused.artifact_format.as_str())
    .bind(reused.process_count)
    .bind(reused.flow_count)
    .bind(reused.impact_count)
    .bind(reused.a_nnz)
    .bind(reused.b_nnz)
    .bind(reused.c_nnz)
    .bind(serde_json::to_value(&reused.coverage)?)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

fn write_report_files(
    report_dir: &PathBuf,
    snapshot_id: Uuid,
    config: &SnapshotBuildConfig,
    coverage: &SnapshotCoverageReport,
    artifact_url: &str,
    source_summary: &SourceSnapshotSummary,
    source_fingerprint: &str,
    build_timing: &BuildTimingSec,
) -> anyhow::Result<()> {
    fs::create_dir_all(report_dir)?;
    let json_path = report_dir.join(format!("{snapshot_id}.json"));
    let md_path = report_dir.join(format!("{snapshot_id}.md"));
    let generated_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let doc = serde_json::json!({
        "snapshot_id": snapshot_id,
        "generated_at_utc": generated_at,
        "config": config,
        "source": {
            "fingerprint": source_fingerprint,
            "summary": source_summary,
        },
        "build_timing_sec": build_timing,
        "coverage": coverage,
        "artifact": {
            "url": artifact_url,
        }
    });
    fs::write(&json_path, serde_json::to_vec_pretty(&doc)?)?;

    let mut md = String::new();
    md.push_str("# Snapshot Coverage Report\n\n");
    md.push_str(&format!("- snapshot_id: `{snapshot_id}`\n"));
    md.push_str(&format!("- generated_at_utc: `{generated_at}`\n"));
    md.push_str(&format!("- process_states: `{}`\n", config.process_states));
    md.push_str(&format!(
        "- include_user_id: `{}`\n",
        config
            .include_user_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string())
    ));
    md.push_str(&format!("- process_limit: `{}`\n", config.process_limit));
    md.push_str(&format!("- provider_rule: `{}`\n", config.provider_rule));
    md.push_str(&format!(
        "- reference_normalization_mode: `{}`\n",
        config.reference_normalization_mode
    ));
    md.push_str(&format!(
        "- allocation_fraction_mode: `{}`\n",
        config.allocation_fraction_mode
    ));
    md.push_str(&format!(
        "- biosphere_sign_mode: `{}`\n",
        config.biosphere_sign_mode
    ));
    md.push_str(&format!(
        "- self_loop_cutoff: `{}`\n",
        config.self_loop_cutoff
    ));
    md.push_str(&format!("- singular_eps: `{}`\n", config.singular_eps));
    md.push_str(&format!("- has_lcia: `{}`\n", config.has_lcia));
    let method_desc = if !config.has_lcia {
        "disabled".to_owned()
    } else if let Some(method_id) = config.method_id {
        format!(
            "{}@{}",
            method_id,
            config.method_version.as_deref().unwrap_or("unknown")
        )
    } else {
        "all_methods".to_owned()
    };
    md.push_str(&format!("- method: `{method_desc}`\n"));
    md.push_str(&format!("- source_fingerprint: `{source_fingerprint}`\n"));
    md.push_str(&format!("- artifact_url: `{artifact_url}`\n\n"));

    md.push_str("## Build Timing (sec)\n\n");
    md.push_str(&format!(
        "- reused_snapshot: `{}`\n",
        build_timing.reused_snapshot
    ));
    md.push_str(&format!("- total_sec: `{}`\n", build_timing.total_sec));
    md.push_str(&format!(
        "- resolve_method_identity_sec: `{}`\n",
        build_timing.resolve_method_identity_sec
    ));
    md.push_str(&format!(
        "- compute_source_fingerprint_sec: `{}`\n",
        build_timing.compute_source_fingerprint_sec
    ));
    md.push_str(&format!(
        "- reuse_lookup_sec: `{}`\n",
        build_timing.reuse_lookup_sec
    ));
    md.push_str(&format!(
        "- load_method_factors_sec: `{}`\n",
        build_timing.load_method_factors_sec
    ));
    md.push_str(&format!(
        "- build_sparse_payload_sec: `{}`\n",
        build_timing.build_sparse_payload_sec
    ));
    md.push_str(&format!(
        "- encode_artifact_sec: `{}`\n",
        build_timing.encode_artifact_sec
    ));
    md.push_str(&format!(
        "- upload_artifact_sec: `{}`\n",
        build_timing.upload_artifact_sec
    ));
    md.push_str(&format!(
        "- persist_metadata_sec: `{}`\n\n",
        build_timing.persist_metadata_sec
    ));

    md.push_str("## Source Summary\n\n");
    md.push_str(&format!(
        "- processes_count: `{}`\n",
        source_summary.process_count
    ));
    md.push_str(&format!(
        "- processes_max_modified_at_utc: `{}`\n",
        source_summary.process_max_modified_at_utc
    ));
    md.push_str(&format!("- flows_count: `{}`\n", source_summary.flow_count));
    md.push_str(&format!(
        "- flows_max_modified_at_utc: `{}`\n",
        source_summary.flow_max_modified_at_utc
    ));
    md.push_str(&format!(
        "- lciamethods_count: `{}`\n",
        source_summary.lciamethod_count
    ));
    md.push_str(&format!(
        "- lciamethods_max_modified_at_utc: `{}`\n\n",
        source_summary.lciamethod_max_modified_at_utc
    ));

    md.push_str("## Matching Coverage\n\n");
    md.push_str(&format!(
        "- input_edges_total: `{}`\n",
        coverage.matching.input_edges_total
    ));
    md.push_str(&format!(
        "- matched_unique_provider: `{}`\n",
        coverage.matching.matched_unique_provider
    ));
    md.push_str(&format!(
        "- matched_multi_provider: `{}`\n",
        coverage.matching.matched_multi_provider
    ));
    md.push_str(&format!(
        "- matched_multi_resolved: `{}`\n",
        coverage.matching.matched_multi_resolved
    ));
    md.push_str(&format!(
        "- matched_multi_unresolved: `{}`\n",
        coverage.matching.matched_multi_unresolved
    ));
    md.push_str(&format!(
        "- matched_multi_fallback_equal: `{}`\n",
        coverage.matching.matched_multi_fallback_equal
    ));
    md.push_str(&format!(
        "- unmatched_no_provider: `{}`\n",
        coverage.matching.unmatched_no_provider
    ));
    md.push_str(&format!(
        "- a_input_edges_written: `{}`\n",
        coverage.matching.a_input_edges_written
    ));
    md.push_str(&format!(
        "- unique_provider_match_pct: `{}`\n",
        coverage.matching.unique_provider_match_pct
    ));
    md.push_str(&format!(
        "- any_provider_match_pct: `{}`\n\n",
        coverage.matching.any_provider_match_pct
    ));

    md.push_str("## Reference Coverage\n\n");
    md.push_str(&format!(
        "- process_total: `{}`\n",
        coverage.reference.process_total
    ));
    md.push_str(&format!(
        "- normalized_process_count: `{}`\n",
        coverage.reference.normalized_process_count
    ));
    md.push_str(&format!(
        "- missing_reference_count: `{}`\n",
        coverage.reference.missing_reference_count
    ));
    md.push_str(&format!(
        "- invalid_reference_count: `{}`\n\n",
        coverage.reference.invalid_reference_count
    ));

    md.push_str("## Allocation Coverage\n\n");
    md.push_str(&format!(
        "- exchange_total: `{}`\n",
        coverage.allocation.exchange_total
    ));
    md.push_str(&format!(
        "- allocation_fraction_present_pct: `{}`\n",
        coverage.allocation.allocation_fraction_present_pct
    ));
    md.push_str(&format!(
        "- allocation_fraction_missing_count: `{}`\n",
        coverage.allocation.allocation_fraction_missing_count
    ));
    md.push_str(&format!(
        "- allocation_fraction_invalid_count: `{}`\n\n",
        coverage.allocation.allocation_fraction_invalid_count
    ));

    md.push_str("## Singular Risk\n\n");
    md.push_str(&format!(
        "- risk_level: `{}`\n",
        coverage.singular_risk.risk_level
    ));
    md.push_str(&format!(
        "- prefilter_diag_abs_ge_cutoff: `{}`\n",
        coverage.singular_risk.prefilter_diag_abs_ge_cutoff
    ));
    md.push_str(&format!(
        "- postfilter_a_diag_abs_ge_cutoff: `{}`\n",
        coverage.singular_risk.postfilter_a_diag_abs_ge_cutoff
    ));
    md.push_str(&format!(
        "- m_zero_diagonal_count: `{}`\n",
        coverage.singular_risk.m_zero_diagonal_count
    ));
    md.push_str(&format!(
        "- m_min_abs_diagonal: `{}`\n\n",
        coverage.singular_risk.m_min_abs_diagonal
    ));

    md.push_str("## Matrix Scale\n\n");
    md.push_str(&format!(
        "- process_count (n): `{}`\n",
        coverage.matrix_scale.process_count
    ));
    md.push_str(&format!(
        "- flow_count: `{}`\n",
        coverage.matrix_scale.flow_count
    ));
    md.push_str(&format!(
        "- impact_count: `{}`\n",
        coverage.matrix_scale.impact_count
    ));
    md.push_str(&format!("- a_nnz: `{}`\n", coverage.matrix_scale.a_nnz));
    md.push_str(&format!("- b_nnz: `{}`\n", coverage.matrix_scale.b_nnz));
    md.push_str(&format!("- c_nnz: `{}`\n", coverage.matrix_scale.c_nnz));
    md.push_str(&format!(
        "- m_nnz_estimated: `{}`\n",
        coverage.matrix_scale.m_nnz_estimated
    ));
    md.push_str(&format!(
        "- m_sparsity_estimated: `{}`\n",
        coverage.matrix_scale.m_sparsity_estimated
    ));

    fs::write(md_path, md)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AllocationMode, ExchangeDirection, NormalizationMode, ParsedExchange, ProcessMeta,
        ProviderRule, add_technosphere_edge, biosphere_gross_value, geo_score,
        resolve_allocation_fraction, resolve_multi_provider, resolve_reference_normalization,
        time_score,
    };
    use serde_json::json;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn assert_close(actual: f64, expected: f64) {
        let delta = (actual - expected).abs();
        assert!(
            delta <= 1e-12,
            "expected {expected}, got {actual}, delta={delta}"
        );
    }

    #[test]
    fn technosphere_edge_is_provider_to_consumer() {
        let mut a_map: HashMap<(i32, i32), f64> = HashMap::new();
        add_technosphere_edge(&mut a_map, 10, 20, 0.4);

        assert_close(*a_map.get(&(10, 20)).expect("provider->consumer"), 0.4);
        assert!(!a_map.contains_key(&(20, 10)));
    }

    #[test]
    fn provider_rule_parse_supports_new_modes() {
        assert_eq!(
            ProviderRule::parse("strict_unique_provider").expect("parse"),
            ProviderRule::StrictUniqueProvider
        );
        assert_eq!(
            ProviderRule::parse("best_provider_strict").expect("parse"),
            ProviderRule::BestProviderStrict
        );
        assert_eq!(
            ProviderRule::parse("split_by_evidence").expect("parse"),
            ProviderRule::SplitByEvidenceStrict
        );
        assert_eq!(
            ProviderRule::parse("split_by_evidence_hybrid").expect("parse"),
            ProviderRule::SplitByEvidenceHybrid
        );
        assert_eq!(
            ProviderRule::parse("split_equal").expect("parse"),
            ProviderRule::SplitEqual
        );
    }

    #[test]
    fn geo_score_prefers_subnational_match() {
        assert_close(geo_score(Some("CN-BJ"), Some("CN-BJ")), 1.0);
        assert_close(geo_score(Some("CN-BJ"), Some("CN-SH")), 0.85);
        assert_close(geo_score(Some("CN"), Some("GLO")), 0.4);
    }

    #[test]
    fn time_score_handles_missing_and_thresholds() {
        assert_close(time_score(None, Some(2020)), 0.5);
        assert_close(time_score(Some(2026), Some(2026)), 1.0);
        assert_close(time_score(Some(2026), Some(2024)), 0.85);
        assert_close(time_score(Some(2026), Some(2016)), 0.4);
        assert_close(time_score(Some(2026), Some(2010)), 0.2);
    }

    #[test]
    fn best_provider_strict_selects_single_top_candidate() {
        let process_meta = vec![
            ProcessMeta {
                process_idx: 0,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: None,
                location: Some("CN-BJ".to_owned()),
                reference_year: Some(2026),
            },
            ProcessMeta {
                process_idx: 1,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: None,
                location: Some("CN-BJ".to_owned()),
                reference_year: Some(2026),
            },
            ProcessMeta {
                process_idx: 2,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: None,
                location: Some("GLO".to_owned()),
                reference_year: Some(2010),
            },
        ];
        let exchange = ParsedExchange {
            process_idx: 0,
            flow_id: Uuid::new_v4(),
            direction: ExchangeDirection::Input,
            amount: Some(1.0),
        };

        let resolution = resolve_multi_provider(
            ProviderRule::BestProviderStrict,
            &exchange,
            &[1, 2],
            &process_meta,
        )
        .expect("resolve")
        .expect("resolved");
        assert_eq!(resolution.allocations.len(), 1);
        assert_eq!(resolution.allocations[0].0, 1);
        assert_close(resolution.allocations[0].1, 1.0);
    }

    #[test]
    fn best_provider_strict_prefers_same_model_id_before_geo_time() {
        let model_consumer = Uuid::new_v4();
        let model_other = Uuid::new_v4();
        let process_meta = vec![
            ProcessMeta {
                process_idx: 0,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: Some(model_consumer),
                location: Some("CN-BJ".to_owned()),
                reference_year: Some(2026),
            },
            ProcessMeta {
                process_idx: 1,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: Some(model_consumer),
                location: Some("CN".to_owned()),
                reference_year: Some(2024),
            },
            ProcessMeta {
                process_idx: 2,
                process_id: Uuid::new_v4(),
                process_version: "01.00.000".to_owned(),
                process_name: None,
                model_id: Some(model_other),
                location: Some("CN-BJ".to_owned()),
                reference_year: Some(2026),
            },
        ];
        let exchange = ParsedExchange {
            process_idx: 0,
            flow_id: Uuid::new_v4(),
            direction: ExchangeDirection::Input,
            amount: Some(1.0),
        };

        let resolution = resolve_multi_provider(
            ProviderRule::BestProviderStrict,
            &exchange,
            &[1, 2],
            &process_meta,
        )
        .expect("resolve")
        .expect("resolved");
        assert_eq!(resolution.allocations.len(), 1);
        assert_eq!(resolution.allocations[0].0, 1);
        assert_close(resolution.allocations[0].1, 1.0);
    }

    #[test]
    fn allocation_fraction_parses_percent_and_numeric() {
        let exchange_percent = json!({
            "allocations": { "allocation": { "@allocatedFraction": "25%" } }
        });
        let exchange_numeric = json!({
            "allocations": { "allocation": { "@allocatedFraction": "25" } }
        });
        let (fraction_percent, _) =
            resolve_allocation_fraction(&exchange_percent, AllocationMode::Strict).expect("parse");
        let (fraction_numeric, _) =
            resolve_allocation_fraction(&exchange_numeric, AllocationMode::Strict).expect("parse");
        assert_close(fraction_percent, 0.25);
        assert_close(fraction_numeric, 0.25);
    }

    #[test]
    fn allocation_fraction_strict_fails_when_missing() {
        let exchange = json!({});
        let err =
            resolve_allocation_fraction(&exchange, AllocationMode::Strict).expect_err("expected");
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn quantitative_reference_normalization_uses_reference_exchange() {
        let process_id = Uuid::new_v4();
        let process_json = json!({
            "processDataSet": {
                "processInformation": {
                    "quantitativeReference": {
                        "referenceToReferenceFlow": "2"
                    }
                }
            }
        });
        let exchanges = [
            json!({
                "@dataSetInternalID": "1",
                "meanAmount": "0.2"
            }),
            json!({
                "@dataSetInternalID": "2",
                "meanAmount": "0.5"
            }),
        ];
        let exchange_refs = exchanges.iter().collect::<Vec<_>>();
        let (scale, stats) = resolve_reference_normalization(
            process_id,
            &process_json,
            exchange_refs.as_slice(),
            NormalizationMode::Strict,
        )
        .expect("normalize");
        assert_close(scale, 2.0);
        assert_eq!(stats.normalized_processes, 1);
        assert_eq!(stats.missing_reference, 0);
        assert_eq!(stats.invalid_reference, 0);
    }

    #[test]
    fn biosphere_gross_value_preserves_input_sign() {
        assert_close(biosphere_gross_value(5.0), 5.0);
        assert_close(biosphere_gross_value(-5.0), -5.0);
        assert_close(biosphere_gross_value(0.0), 0.0);
    }
}
