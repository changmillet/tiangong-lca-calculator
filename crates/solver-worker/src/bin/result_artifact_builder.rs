//! Build query artifacts independently of successful calculation and release ZIP generation.
use clap::{Parser, Subcommand};
use solver_worker::calculation_bundle::{
    CalculationBundleArtifactRef,
    query_artifact::{self, BuildLimits, QueryArtifactRef, transport},
};
use std::{fs::File, path::PathBuf};
use uuid::Uuid;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Convert a trusted canonical bundle into a new local numeric query artifact.
    Build {
        /// Trusted `CalculationBundleArtifactRef` JSON. Never an arbitrary source URL.
        #[arg(long)]
        bundle_ref: PathBuf,
        /// Local canonical bundle directory; omit to fetch from configured S3.
        #[arg(long)]
        source_dir: Option<PathBuf>,
        /// New output directory; existing destinations are never overwritten.
        #[arg(long)]
        output: PathBuf,
        /// Identity allocated for this encoding attempt.
        #[arg(long)]
        artifact_id: Uuid,
        /// Upload verified files and manifest to configured S3 after local persistence.
        #[arg(long)]
        upload: bool,
    },
    /// Retry upload of an existing artifact without rebuilding or recalculating.
    Upload {
        #[arg(long)]
        artifact_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Args::parse().command {
        Command::Build {
            bundle_ref,
            source_dir,
            output,
            artifact_id,
            upload,
        } => {
            anyhow::ensure!(
                std::fs::metadata(&bundle_ref)?.len() <= query_artifact::MAX_MANIFEST_BYTES,
                "bundle reference exceeds limit"
            );
            let reference: CalculationBundleArtifactRef =
                serde_json::from_reader(File::open(bundle_ref)?)?;
            let limits = BuildLimits::default();
            let store = if source_dir.is_none() || upload {
                Some(transport::store_from_env()?)
            } else {
                None
            };
            let downloaded = if source_dir.is_none() {
                Some(
                    transport::download_bundle(
                        store.as_ref().expect("configured store"),
                        &reference,
                        &limits,
                    )
                    .await?,
                )
            } else {
                None
            };
            let source = source_dir.unwrap_or_else(|| {
                downloaded
                    .as_ref()
                    .expect("downloaded source")
                    .path()
                    .to_owned()
            });
            let artifact = tokio::task::spawn_blocking(move || {
                query_artifact::build(&source, &reference, artifact_id, &limits)
            })
            .await??;
            artifact.persist(&output)?;
            if upload {
                transport::upload(store.as_ref().expect("configured store"), &artifact).await?;
            }
            println!("{}", serde_json::to_string(&artifact.reference)?);
        }
        Command::Upload { artifact_dir } => {
            let path = artifact_dir.join("artifact-ref.json");
            anyhow::ensure!(
                std::fs::metadata(&path)?.len() <= query_artifact::MAX_MANIFEST_BYTES,
                "query receipt exceeds limit"
            );
            let reference: QueryArtifactRef = serde_json::from_reader(File::open(path)?)?;
            let reference = transport::upload_directory(
                &transport::store_from_env()?,
                &artifact_dir,
                &reference,
            )
            .await?;
            println!("{}", serde_json::to_string(&reference)?);
        }
    }
    Ok(())
}
