//! Offline candidate only: no queue, database, actor authority or study approval.
use anyhow::{Context, ensure};
use clap::Parser;
use solver_worker::{
    static_lcia_cache::TrustedStaticCacheSource,
    study::{
        contracts::Bounds,
        method_assessment::{TrustedMethods, assess_candidate, verify_output},
        method_assessment_contracts::CandidateInput,
        sha256,
    },
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    prepared: PathBuf,
    #[arg(long)]
    result: PathBuf,
    #[arg(long)]
    source_snapshot: PathBuf,
    #[arg(long)]
    static_cache: PathBuf,
    #[arg(long)]
    output: PathBuf,
}
fn read_regular(path: &Path, maximum: usize) -> anyhow::Result<Vec<u8>> {
    let before = std::fs::symlink_metadata(path)?;
    ensure!(
        before.is_file() && !before.file_type().is_symlink(),
        "method candidate input must be a regular nonsymlink file"
    );
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file: File = options.open(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.len() <= u64::try_from(maximum)?,
        "method candidate input exceeds its regular-file byte bound"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            before.dev() == opened.dev() && before.ino() == opened.ino(),
            "method candidate input changed during open"
        );
    }
    let mut bytes = Vec::new();
    file.take(
        u64::try_from(maximum)?
            .checked_add(1)
            .context("method candidate size overflow")?,
    )
    .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= maximum, "method candidate input byte limit");
    Ok(bytes)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let bounds = Bounds::default();
    let input: CandidateInput =
        solver_worker::study::json::from_slice(&read_regular(&args.input, 262_144)?)?;
    solver_worker::study::method_assessment::validate_candidate(&input)?;
    let prepared = read_regular(&args.prepared, bounds.max_artifact_bytes)?;
    let result = read_regular(&args.result, bounds.max_artifact_bytes)?;
    let snapshot = read_regular(&args.source_snapshot, bounds.max_source_bytes)?;
    let methods =
        TrustedMethods::load(&TrustedStaticCacheSource::Directory(args.static_cache)).await?;
    let facts = assess_candidate(&input, &prepared, &result, &snapshot, &methods, &bounds)?;
    let bytes = serde_json::to_vec(&facts)?;
    verify_output(&facts, &bytes, bounds.max_artifact_bytes)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&args.output)?;
    // Never overwrite or unlink a pre-existing/user replacement path. A failed
    // write leaves its exclusive partial file for explicit operator disposition.
    output.write_all(&bytes)?;
    output.sync_all()?;
    println!(
        "{}",
        serde_json::json!({"schemaVersion":"lca.study-method-candidate-receipt.v1","classification":"offline_candidate_not_registered_or_authorized","candidateId":input.candidate_id,"profileId":facts.profile_id,"factsSha256":sha256(&bytes),"byteSize":bytes.len(),"requirements":facts.requirements.len(),"normalisationWeighting":facts.candidate_normalisation_weighting.status,"outcome":facts.outcome,"pefConformityQualified":false,"scientificRankingQualified":false,"businessPilotQualified":false})
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn regular_file_bound_is_enforced_before_unbounded_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.json");
        std::fs::write(&path, b"12345").unwrap();
        assert!(read_regular(&path, 4).is_err());
        assert_eq!(read_regular(&path, 5).unwrap(), b"12345");
    }
    #[cfg(unix)]
    #[test]
    fn fifo_and_symlink_controls_refuse_without_opening_a_stream() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("input.fifo");
        let result = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(read_regular(&fifo, 10).is_err());
        let path = dir.path().join("link");
        symlink(&fifo, &path).unwrap();
        assert!(read_regular(&path, 10).is_err());
    }
}
