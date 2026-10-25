//! Inspect the local calculation identity, or explicitly activate it with compare-and-set.
use clap::Parser;
use solver_worker::{calculation_contract, db_pool::WorkerDbPoolOptions, pgbouncer_sqlx as sqlx};

#[derive(Parser)]
struct Args {
    /// Explicitly activate this binary's descriptor. Never called by normal Worker startup.
    #[arg(long)]
    activate: bool,
    /// Exact current identity; omit only for the initial, unconfigured activation.
    #[arg(long)]
    expected_fingerprint: Option<String>,
    #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.activate {
        println!(
            "{}",
            serde_json::json!({"fingerprint": calculation_contract::fingerprint(), "descriptor": calculation_contract::descriptor()})
        );
        return Ok(());
    }
    let url = args
        .database_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("DATABASE_URL is required for activation"))?;
    let pool = WorkerDbPoolOptions::new("calculation-contract-activation")
        .max_connections(1)
        .connect(url)
        .await?;
    let result: serde_json::Value = sqlx::query_scalar(
        "WITH service AS (SELECT set_config('request.jwt.claim.role', 'service_role', true)) SELECT private.svc_lca_calculation_contract_activate($1, $2) FROM service"
    ).bind(calculation_contract::descriptor()).bind(args.expected_fingerprint).fetch_one(&pool).await?;
    anyhow::ensure!(
        result.get("ok").and_then(serde_json::Value::as_bool) == Some(true),
        "calculation activation refused: {result}"
    );
    anyhow::ensure!(
        result
            .get("fingerprint")
            .and_then(serde_json::Value::as_str)
            == Some(calculation_contract::fingerprint()),
        "activated calculation identity differs from this binary"
    );
    println!("{result}");
    Ok(())
}
