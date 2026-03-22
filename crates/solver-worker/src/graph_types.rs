use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotSelectionMode {
    #[default]
    FilteredLibrary,
    RequestRootsClosure,
}

impl SnapshotSelectionMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FilteredLibrary => "filtered_library",
            Self::RequestRootsClosure => "request_roots_closure",
        }
    }
}

impl fmt::Display for SnapshotSelectionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeProcessPartition {
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RequestRootProcess {
    pub process_id: Uuid,
    pub process_version: String,
}

impl RequestRootProcess {
    #[must_use]
    pub fn new(process_id: Uuid, process_version: impl Into<String>) -> Self {
        Self {
            process_id,
            process_version: process_version.into().trim().to_owned(),
        }
    }
}

impl fmt::Display for RequestRootProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.process_id, self.process_version)
    }
}

impl FromStr for RequestRootProcess {
    type Err = anyhow::Error;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        let Some((process_id, process_version)) = trimmed.split_once('@') else {
            return Err(anyhow::anyhow!(
                "invalid request root '{trimmed}'; expected <uuid>@<version>"
            ));
        };
        let process_id = process_id
            .trim()
            .parse::<Uuid>()
            .map_err(|err| anyhow::anyhow!("invalid request root process id: {err}"))?;
        let process_version = process_version.trim();
        if process_version.is_empty() {
            return Err(anyhow::anyhow!(
                "invalid request root '{trimmed}'; missing process version"
            ));
        }
        Ok(Self::new(process_id, process_version.to_owned()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedScopeProcess {
    pub process_id: Uuid,
    pub process_version: String,
    pub partition: ScopeProcessPartition,
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{RequestRootProcess, SnapshotSelectionMode};

    #[test]
    fn request_root_process_parses_uuid_and_version() {
        let process_id = Uuid::new_v4();
        let parsed: RequestRootProcess = format!("{process_id}@01.00.000")
            .parse()
            .expect("parse request root");
        assert_eq!(parsed.process_id, process_id);
        assert_eq!(parsed.process_version, "01.00.000");
        assert_eq!(parsed.to_string(), format!("{process_id}@01.00.000"));
    }

    #[test]
    fn request_root_process_rejects_missing_version() {
        let process_id = Uuid::new_v4();
        let error = format!("{process_id}@")
            .parse::<RequestRootProcess>()
            .expect_err("missing version should fail");
        assert!(error.to_string().contains("missing process version"));
    }

    #[test]
    fn snapshot_selection_mode_default_is_filtered_library() {
        assert_eq!(
            SnapshotSelectionMode::default(),
            SnapshotSelectionMode::FilteredLibrary
        );
        assert_eq!(
            SnapshotSelectionMode::RequestRootsClosure.to_string(),
            "request_roots_closure"
        );
    }
}
