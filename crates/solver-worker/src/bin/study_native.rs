//! Restricted child entrypoint. Never emits source documents, credentials or raw errors.
use clap::Parser;
use solver_worker::study::{
    runtime_child::execute_request,
    runtime_contracts::{NativeErrorEnvelope, NativeErrorKind, NativeRequest},
};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    request: std::path::PathBuf,
}
#[tokio::main(worker_threads = 2)]
async fn main() {
    let args = Args::parse();
    let result = async {
        anyhow::ensure!(args.request.is_absolute(), "study_child_path_invalid");
        anyhow::ensure!(
            std::fs::metadata(&args.request)?.len() <= 4 * 1024 * 1024,
            "study_child_request_limit"
        );
        let request: NativeRequest =
            solver_worker::study::json::from_slice(&std::fs::read(args.request)?)?;
        let output = std::path::PathBuf::from(&request.output_path);
        let outcome = execute_request(request).await;
        if let Err(error) = &outcome {
            let label = error.to_string();
            let kind = if label.starts_with("study_undeclared_boundary") {
                NativeErrorKind::BoundaryUndeclared
            } else if label.starts_with("study_normalization_underflow") {
                NativeErrorKind::NormalizationUnderflow
            } else if label.starts_with("study_normalization_overflow") {
                NativeErrorKind::NormalizationOverflow
            } else if label.starts_with("study_") && label.contains("_limit") {
                NativeErrorKind::ResourceLimit
            } else if label.starts_with("study_negative_activity")
                || label.starts_with("study_balance_residual")
                || label.starts_with("study_nonfinite")
            {
                NativeErrorKind::NumericInvalid
            } else if error.downcast_ref::<solver_core::SolverError>().is_some() {
                NativeErrorKind::ModelNotSolvable
            } else {
                NativeErrorKind::SourceInvalid
            };
            if let Ok(bytes) = serde_json::to_vec(&NativeErrorEnvelope {
                schema_version: "lca.study-native.failure.v1".into(),
                kind,
            }) {
                let _ = std::fs::write(output, bytes);
            }
        }
        outcome
    }
    .await;
    if result.is_err() {
        eprintln!("study_native_execution_failed");
        std::process::exit(1);
    }
}
