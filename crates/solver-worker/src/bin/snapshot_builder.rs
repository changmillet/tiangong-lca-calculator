#![allow(
    clippy::cast_precision_loss,
    clippy::collapsible_if,
    clippy::comparison_chain,
    clippy::format_push_string,
    clippy::needless_raw_string_hashes,
    clippy::reserve_after_initialization,
    clippy::too_many_arguments,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use clap::Parser;
use serde_json::Value;
use sha2::{Digest, Sha256};
use solver_core::{ModelSparseData, SparseTriplet};
use solver_worker::snapshot_artifacts::{
    SnapshotBuildConfig, SnapshotCoverageReport, SnapshotMatchingCoverage, SnapshotMatrixScale,
    SnapshotSingularRisk, encode_snapshot_artifact,
};
use solver_worker::storage::ObjectStoreClient;
use sqlx::{PgPool, Row};
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
    #[arg(long, default_value_t = 0)]
    process_limit: usize,
    #[arg(long, default_value = "strict_unique_provider")]
    provider_rule: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ProcessKey {
    id: Uuid,
    version: String,
}

#[derive(Debug, Clone)]
struct ProcessRow {
    key: ProcessKey,
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

#[derive(Debug, Clone)]
struct MethodSelection {
    has_lcia: bool,
    method_id: Option<Uuid>,
    method_version: Option<String>,
    factor_map: HashMap<Uuid, f64>,
}

#[derive(Debug, Clone)]
struct BuildOutput {
    data: ModelSparseData,
    coverage: SnapshotCoverageReport,
    source_hash: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.provider_rule != "strict_unique_provider" {
        return Err(anyhow::anyhow!(
            "only strict_unique_provider is supported in snapshot-builder"
        ));
    }

    let db_url = cli
        .database_url
        .as_deref()
        .or(cli.conn.as_deref())
        .ok_or_else(|| anyhow::anyhow!("missing DB connection: set DATABASE_URL or CONN"))?;
    let pool = PgPool::connect(db_url).await?;

    let store = build_object_store(&cli)?;
    let snapshot_id = cli.snapshot_id.unwrap_or_else(Uuid::new_v4);
    let (all_states, state_codes, process_states_label) =
        parse_process_states(&cli.process_states)?;
    let method = resolve_method(&pool, &cli).await?;

    println!("[info] snapshot_id={snapshot_id}");
    println!("[info] process_states={process_states_label}");
    println!("[info] process_limit={}", cli.process_limit);
    println!("[info] provider_rule={}", cli.provider_rule);
    println!("[info] self_loop_cutoff={}", cli.self_loop_cutoff);
    println!("[info] singular_eps={}", cli.singular_eps);
    if method.has_lcia {
        println!(
            "[info] lcia_method={}@{} factors={}",
            method.method_id.expect("method id"),
            method.method_version.as_deref().unwrap_or_default(),
            method.factor_map.len()
        );
    } else {
        println!("[info] lcia_method=disabled");
    }

    let built = build_sparse_payload(
        &pool,
        snapshot_id,
        all_states,
        &state_codes,
        cli.process_limit,
        cli.self_loop_cutoff,
        cli.singular_eps,
        &method,
    )
    .await?;

    let build_config = SnapshotBuildConfig {
        process_states: process_states_label.clone(),
        process_limit: i32::try_from(cli.process_limit)
            .map_err(|_| anyhow::anyhow!("process_limit overflow"))?,
        provider_rule: cli.provider_rule.clone(),
        self_loop_cutoff: cli.self_loop_cutoff,
        singular_eps: cli.singular_eps,
        has_lcia: method.has_lcia,
        method_id: method.method_id,
        method_version: method.method_version.clone(),
    };

    let encoded = encode_snapshot_artifact(
        snapshot_id,
        build_config.clone(),
        built.coverage.clone(),
        &built.data,
    )?;
    let artifact_url = store
        .upload_snapshot_artifact(
            snapshot_id,
            encoded.extension,
            encoded.content_type,
            encoded.bytes,
        )
        .await?;

    persist_snapshot_metadata(
        &pool,
        snapshot_id,
        &cli.provider_rule,
        all_states,
        &state_codes,
        &method,
        &built,
        &artifact_url,
        &encoded.sha256,
        i64::try_from(encoded.byte_size).map_err(|_| anyhow::anyhow!("artifact too large"))?,
        encoded.format,
    )
    .await?;

    write_report_files(
        &cli.report_dir,
        snapshot_id,
        &build_config,
        &built.coverage,
        &artifact_url,
    )?;

    println!("[done] snapshot ready: {snapshot_id}");
    println!("[artifact] {artifact_url}");
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

async fn resolve_method(pool: &PgPool, cli: &Cli) -> anyhow::Result<MethodSelection> {
    if cli.no_lcia {
        return Ok(MethodSelection {
            has_lcia: false,
            method_id: None,
            method_version: None,
            factor_map: HashMap::new(),
        });
    }

    if cli.method_id.is_some() && cli.method_version.is_none() {
        return Err(anyhow::anyhow!(
            "--method-version is required when --method-id is set"
        ));
    }

    let (method_id, method_version) = if let Some(method_id) = cli.method_id {
        (
            method_id,
            cli.method_version
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing method version"))?,
        )
    } else {
        let row = sqlx::query(
            r#"
            WITH m AS (
              SELECT
                id,
                version::text AS version,
                CASE
                  WHEN jsonb_typeof(json#>'{LCIAMethodDataSet,characterisationFactors,factor}') = 'array'
                  THEN jsonb_array_length(json#>'{LCIAMethodDataSet,characterisationFactors,factor}')
                  ELSE 0
                END AS factor_cnt
              FROM public.lciamethods
            )
            SELECT id, version
            FROM m
            ORDER BY factor_cnt DESC, id
            LIMIT 1
            "#,
        )
        .fetch_optional(pool)
        .await?;

        let row = row.ok_or_else(|| anyhow::anyhow!("no lciamethods found"))?;
        (
            row.try_get::<Uuid, _>("id")?,
            row.try_get::<String, _>("version")?,
        )
    };

    let method_json: Value = sqlx::query_scalar(
        "SELECT json FROM public.lciamethods WHERE id = $1 AND version = $2::bpchar",
    )
    .bind(method_id)
    .bind(method_version.clone())
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow::anyhow!("lciamethod not found: {}@{}", method_id, method_version))?;

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

    Ok(MethodSelection {
        has_lcia: true,
        method_id: Some(method_id),
        method_version: Some(method_version),
        factor_map,
    })
}

async fn build_sparse_payload(
    pool: &PgPool,
    snapshot_id: Uuid,
    all_states: bool,
    state_codes: &[i32],
    process_limit: usize,
    self_loop_cutoff: f64,
    singular_eps: f64,
    method: &MethodSelection,
) -> anyhow::Result<BuildOutput> {
    let mut processes = fetch_processes(pool, all_states, state_codes).await?;
    if process_limit > 0 && processes.len() > process_limit {
        processes.truncate(process_limit);
    }
    if processes.is_empty() {
        return Err(anyhow::anyhow!("no processes matched filter"));
    }

    let mut process_keys = Vec::with_capacity(processes.len());
    let mut process_idx_by_key = HashMap::with_capacity(processes.len());
    for (idx, proc_row) in processes.iter().enumerate() {
        let idx_i32 = i32::try_from(idx).map_err(|_| anyhow::anyhow!("process index overflow"))?;
        process_idx_by_key.insert(proc_row.key.clone(), idx_i32);
        process_keys.push(proc_row.key.clone());
    }

    let mut exchanges = Vec::<ParsedExchange>::new();
    let mut flow_candidates: BTreeSet<Uuid> = BTreeSet::new();

    for proc_row in &processes {
        let process_idx = *process_idx_by_key
            .get(&proc_row.key)
            .ok_or_else(|| anyhow::anyhow!("missing process index"))?;
        for ex in process_exchange_items(&proc_row.json) {
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
            let amount = parse_number(
                ex.get("meanAmount")
                    .or_else(|| ex.get("resultingAmount"))
                    .or_else(|| ex.get("meanValue")),
            );

            exchanges.push(ParsedExchange {
                process_idx,
                flow_id,
                direction,
                amount,
            });
            flow_candidates.insert(flow_id);
        }
    }

    for flow_id in method.factor_map.keys() {
        flow_candidates.insert(*flow_id);
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

    let mut provider_map: HashMap<Uuid, HashSet<i32>> = HashMap::new();
    for ex in &exchanges {
        if ex.direction == ExchangeDirection::Output {
            provider_map
                .entry(ex.flow_id)
                .or_default()
                .insert(ex.process_idx);
        }
    }

    let mut a_map: HashMap<(i32, i32), f64> = HashMap::new();
    let mut b_map: HashMap<(i32, i32), f64> = HashMap::new();
    let mut input_edges_total: i64 = 0;
    let mut matched_unique: i64 = 0;
    let mut matched_multi: i64 = 0;
    let mut unmatched: i64 = 0;

    for ex in &exchanges {
        if let Some(flow_idx) = flow_idx_by_id.get(&ex.flow_id).copied() {
            if elementary_flow_idx.contains(&flow_idx)
                && let Some(amount) = ex.amount
            {
                let value = match ex.direction {
                    ExchangeDirection::Input => -amount,
                    ExchangeDirection::Output => amount,
                };
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
        let provider_cnt = provider_map.get(&ex.flow_id).map_or(0, HashSet::len);
        if provider_cnt == 1 {
            matched_unique += 1;
            let provider_idx = *provider_map
                .get(&ex.flow_id)
                .and_then(|set| set.iter().next())
                .ok_or_else(|| anyhow::anyhow!("missing provider idx"))?;
            *a_map.entry((ex.process_idx, provider_idx)).or_insert(0.0) += amount;
        } else if provider_cnt > 1 {
            matched_multi += 1;
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
    let process_count_i32 =
        i32::try_from(process_keys.len()).map_err(|_| anyhow::anyhow!("process overflow"))?;
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
    if method.has_lcia {
        let mut c_map = HashMap::<i32, f64>::new();
        for (flow_id, cf_value) in &method.factor_map {
            if let Some(flow_idx) = flow_idx_by_id.get(flow_id).copied()
                && cf_value.abs() > f64::EPSILON
            {
                *c_map.entry(flow_idx).or_insert(0.0) += *cf_value;
            }
        }
        c_map.retain(|_, value| value.abs() > f64::EPSILON);
        characterization_factors.reserve(c_map.len());
        for (col, value) in c_map {
            characterization_factors.push(SparseTriplet { row: 0, col, value });
        }
    }

    let impact_count = 1_i32;
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

    let coverage = SnapshotCoverageReport {
        matching: SnapshotMatchingCoverage {
            input_edges_total,
            matched_unique_provider: matched_unique,
            matched_multi_provider: matched_multi,
            unmatched_no_provider: unmatched,
            unique_provider_match_pct,
            any_provider_match_pct,
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

    let source_hash = build_source_hash(
        process_count_i64,
        i64::from(flow_count),
        a_nnz,
        b_nnz,
        c_nnz,
        method,
    );

    Ok(BuildOutput {
        data,
        coverage,
        source_hash,
    })
}

fn pct(numerator: i64, denominator: i64) -> f64 {
    if denominator <= 0 {
        0.0
    } else {
        ((numerator as f64 / denominator as f64) * 10000.0).round() / 100.0
    }
}

fn build_source_hash(
    process_count: i64,
    flow_count: i64,
    a_nnz: i64,
    b_nnz: i64,
    c_nnz: i64,
    method: &MethodSelection,
) -> String {
    let method_part = if method.has_lcia {
        format!(
            "{}:{}",
            method.method_id.expect("method id"),
            method.method_version.as_deref().unwrap_or_default()
        )
    } else {
        "no_lcia".to_owned()
    };
    let body = format!(
        "{process_count}:{flow_count}:{a_nnz}:{b_nnz}:{c_nnz}:strict_unique_provider:{method_part}"
    );
    let mut hasher = Sha256::new();
    hasher.update(body.as_bytes());
    hex::encode(hasher.finalize())
}

async fn fetch_processes(
    pool: &PgPool,
    all_states: bool,
    state_codes: &[i32],
) -> anyhow::Result<Vec<ProcessRow>> {
    let rows = if all_states {
        sqlx::query(
            r#"
            SELECT id, version::text AS version, json
            FROM public.processes
            WHERE json ? 'processDataSet'
            ORDER BY id, version
            "#,
        )
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query(
            r#"
            SELECT id, version::text AS version, json
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
        let id = row.try_get::<Uuid, _>("id")?;
        let version = row.try_get::<String, _>("version")?;
        out.push(ProcessRow {
            key: ProcessKey { id, version },
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
    let candidate_set = flow_candidates.iter().copied().collect::<HashSet<_>>();
    let rows = sqlx::query(
        r#"
        SELECT id, json
        FROM public.flows
        ORDER BY id, state_code DESC, modified_at DESC NULLS LAST, created_at DESC NULLS LAST
        "#,
    )
    .fetch_all(pool)
    .await?;

    let mut out = HashMap::<Uuid, Value>::new();
    for row in rows {
        let id = row.try_get::<Uuid, _>("id")?;
        if !candidate_set.contains(&id) || out.contains_key(&id) {
            continue;
        }
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
    method: &MethodSelection,
    built: &BuildOutput,
    artifact_url: &str,
    artifact_sha256: &str,
    artifact_byte_size: i64,
    artifact_format: &str,
) -> anyhow::Result<()> {
    let process_filter = if all_states {
        serde_json::json!({"all_states": true})
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
    .bind(&built.source_hash)
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

fn write_report_files(
    report_dir: &PathBuf,
    snapshot_id: Uuid,
    config: &SnapshotBuildConfig,
    coverage: &SnapshotCoverageReport,
    artifact_url: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(report_dir)?;
    let json_path = report_dir.join(format!("{snapshot_id}.json"));
    let md_path = report_dir.join(format!("{snapshot_id}.md"));
    let generated_at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let doc = serde_json::json!({
        "snapshot_id": snapshot_id,
        "generated_at_utc": generated_at,
        "config": config,
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
    md.push_str(&format!("- process_limit: `{}`\n", config.process_limit));
    md.push_str(&format!("- provider_rule: `{}`\n", config.provider_rule));
    md.push_str(&format!(
        "- self_loop_cutoff: `{}`\n",
        config.self_loop_cutoff
    ));
    md.push_str(&format!("- singular_eps: `{}`\n", config.singular_eps));
    md.push_str(&format!("- has_lcia: `{}`\n", config.has_lcia));
    md.push_str(&format!(
        "- method: `{}@{}`\n",
        config
            .method_id
            .map_or_else(|| "none".to_owned(), |id| id.to_string()),
        config.method_version.as_deref().unwrap_or("none")
    ));
    md.push_str(&format!("- artifact_url: `{artifact_url}`\n\n"));

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
        "- unmatched_no_provider: `{}`\n",
        coverage.matching.unmatched_no_provider
    ));
    md.push_str(&format!(
        "- unique_provider_match_pct: `{}`\n",
        coverage.matching.unique_provider_match_pct
    ));
    md.push_str(&format!(
        "- any_provider_match_pct: `{}`\n\n",
        coverage.matching.any_provider_match_pct
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
