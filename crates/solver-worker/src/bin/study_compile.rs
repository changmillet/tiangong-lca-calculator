//! Task-owned offline compiler/executor. This is not the W06 hosted job adapter.
use clap::Parser;
use solver_worker::{
    static_lcia_cache::{TrustedStaticCacheSource, load_verified_static_lcia_bundle},
    study::{
        VerifiedMethods, artifact_bytes,
        contracts::{Bounds, SourceInput},
        execute, prepare,
    },
};
use std::path::PathBuf;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    static_cache: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    result: PathBuf,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let bounds = Bounds::default();
    anyhow::ensure!(
        std::fs::metadata(&args.source)?.len() <= u64::try_from(bounds.max_source_bytes)?,
        "study_source_bytes_limit"
    );
    let bytes = std::fs::read(args.source)?;
    let source: SourceInput = serde_json::from_slice(&bytes)?;
    let request = solver_worker::calculation_evidence::method_factor_source_contract_fixture();
    let bundle = load_verified_static_lcia_bundle(
        &TrustedStaticCacheSource::Directory(args.static_cache),
        &request,
    )
    .await?;
    let methods = VerifiedMethods::from_static(&bundle, &source.method_refs)?;
    let prepared = prepare(&bytes, &methods, &bounds)?;
    let result = execute(&prepared)?;
    std::fs::write(args.output, artifact_bytes(&prepared)?)?;
    std::fs::write(args.result, serde_json::to_vec_pretty(&result)?)?;
    println!(
        "{}",
        serde_json::json!({"status":"passed","profile":prepared.profile_id,"modelHash":prepared.model_hash,"inputHash":prepared.input_hash,"views":prepared.numeric.views.len(),"inventoryAxes":prepared.numeric.inventory_axes.len(),"coverageGaps":prepared.coverage.len(),"elapsedMillis":result.elapsed_millis,"businessPilotQualified":false})
    );
    Ok(())
}
