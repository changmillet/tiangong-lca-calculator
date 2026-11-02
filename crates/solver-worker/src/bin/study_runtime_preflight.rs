//! Local executable qualification; never grants a source, model or hosted runtime capability.
use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    native_bin: std::path::PathBuf,
    #[arg(long)]
    directory: std::path::PathBuf,
    #[arg(long)]
    receipt: std::path::PathBuf,
}
#[tokio::main]
async fn main() {
    let args = Args::parse();
    let outcome = async {
        anyhow::ensure!(
            args.directory.is_absolute() && args.receipt.is_absolute(),
            "study_preflight_paths"
        );
        let receipt = solver_worker::study::runtime_preflight::probe(
            &args.native_bin,
            &args.directory,
            tokio::time::Instant::now() + std::time::Duration::from_secs(60),
            &solver_worker::resource::CancellationToken::default(),
        )
        .await?;
        std::fs::write(&args.receipt, serde_json::to_vec_pretty(&receipt)?)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if outcome.is_err() {
        eprintln!("study_native_preflight_failed");
        std::process::exit(1);
    }
    println!(
        "{}",
        serde_json::json!({"status":"passed","scope":"local_native_component_only"})
    );
}
