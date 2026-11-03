//! Privileged scoped reconciliation; never accepts a caller-provided bucket or object locator.
use clap::Parser;
use solver_worker::{config::AppConfig, db::AppState};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    receipt: std::path::PathBuf,
    #[command(flatten)]
    config: AppConfig,
}
#[tokio::main]
async fn main() {
    let args = Args::parse();
    let outcome = async {
        let state = AppState::new(&args.config).await?;
        solver_worker::study::runtime::reconcile(&state, &args.receipt).await
    }
    .await;
    if let Ok(status) = outcome {
        println!("{}", serde_json::json!({"status":status}));
    } else {
        eprintln!("study_orphan_reconciliation_failed");
        std::process::exit(1);
    }
}
