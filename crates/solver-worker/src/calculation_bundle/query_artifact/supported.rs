//! Explicit compatibility dispatch; v1 Process rows and v2 Result rows remain distinct.
use super::{
    FORMAT, QueryArtifactRef, QueryFile, QueryManifest, QueryReader,
    final_result::{FinalManifest, FinalQueryReader},
};
use std::{fs::File, path::Path};
pub enum ArtifactManifest {
    Legacy(QueryManifest),
    Final(FinalManifest),
}
impl ArtifactManifest {
    pub fn read(path: &Path, reference: &QueryArtifactRef) -> anyhow::Result<Self> {
        reference.validate()?;
        Ok(match reference.schema_version.as_str() {
            FORMAT => {
                let value = serde_json::from_reader(File::open(path)?)?;
                super::reader::validate_manifest(&value, reference)?;
                Self::Legacy(value)
            }
            super::final_result::FORMAT => {
                let value: FinalManifest = serde_json::from_reader(File::open(path)?)?;
                value.validate(reference)?;
                Self::Final(value)
            }
            _ => anyhow::bail!("unsupported query format"),
        })
    }
    #[must_use]
    pub fn files(&self) -> &[QueryFile] {
        match self {
            Self::Legacy(v) => &v.files,
            Self::Final(v) => &v.files,
        }
    }
    #[must_use]
    pub fn row_count(&self) -> usize {
        match self {
            Self::Legacy(v) => v.process_count,
            Self::Final(v) => v.result_count,
        }
    }
}
pub enum ArtifactReader {
    Legacy(QueryReader),
    Final(FinalQueryReader),
}
impl ArtifactReader {
    pub fn open(root: &Path, reference: &QueryArtifactRef) -> anyhow::Result<Self> {
        Ok(match reference.schema_version.as_str() {
            FORMAT => Self::Legacy(QueryReader::open(root, reference)?),
            super::final_result::FORMAT => Self::Final(FinalQueryReader::open(root, reference)?),
            _ => anyhow::bail!("unsupported query format"),
        })
    }
    #[must_use]
    pub fn files(&self) -> &[QueryFile] {
        match self {
            Self::Legacy(v) => &v.manifest.files,
            Self::Final(v) => &v.manifest.files,
        }
    }
    pub fn validate_values(&self) -> anyhow::Result<()> {
        match self {
            Self::Legacy(v) => v.validate_values(),
            Self::Final(v) => v.validate_values(),
        }
    }
}
