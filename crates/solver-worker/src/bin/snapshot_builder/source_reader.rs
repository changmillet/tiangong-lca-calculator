//! Transport boundary for the existing snapshot compiler and source-closure owner.

use super::{
    CompiledReleaseSourceDatasetType, FlowLinkIdentity, FlowReferenceRequests, FlowReleaseMetadata,
    FlowRow, HashMap, MethodRow, PgPool, ResolvedFlowMetadata, SourceDatasetReadIdentity, Uuid,
    ValidatedPublicOwnerDraftScope, fetch_exact_source_dataset_rows, fetch_flow_meta,
    fetch_flow_meta_batched, fetch_flow_release_metadata, fetch_source_dataset_metadata,
    fetch_source_dataset_rows, provider_impact::FrozenSources,
};

#[derive(Clone, Copy)]
pub(super) enum SnapshotSourceReader<'a> {
    Database(&'a PgPool),
    Frozen(&'a FrozenSources),
}

impl SnapshotSourceReader<'_> {
    pub(super) async fn flow_meta(
        self,
        requests: &FlowReferenceRequests,
        scope: Option<&ValidatedPublicOwnerDraftScope>,
    ) -> anyhow::Result<ResolvedFlowMetadata> {
        match self {
            Self::Database(pool) => fetch_flow_meta(pool, requests, scope).await,
            Self::Frozen(sources) => sources.flow_meta(requests),
        }
    }

    pub(super) async fn flow_meta_batched(
        self,
        requests: &FlowReferenceRequests,
        scope: Option<&ValidatedPublicOwnerDraftScope>,
    ) -> anyhow::Result<ResolvedFlowMetadata> {
        match self {
            Self::Database(pool) => fetch_flow_meta_batched(pool, requests, scope).await,
            Self::Frozen(sources) => sources.flow_meta(requests),
        }
    }

    pub(super) async fn flow_release_metadata(
        self,
        flows: &HashMap<FlowLinkIdentity, FlowRow>,
    ) -> anyhow::Result<HashMap<FlowLinkIdentity, FlowReleaseMetadata>> {
        match self {
            Self::Database(pool) => fetch_flow_release_metadata(pool, flows).await,
            Self::Frozen(sources) => sources.flow_release_metadata(flows),
        }
    }

    pub(super) async fn dataset_rows(
        self,
        kind: CompiledReleaseSourceDatasetType,
        ids: &[Uuid],
    ) -> anyhow::Result<Vec<MethodRow>> {
        match self {
            Self::Database(pool) => fetch_source_dataset_rows(pool, kind, ids).await,
            Self::Frozen(sources) => Ok(sources.dataset_rows(kind, ids)),
        }
    }

    pub(super) async fn dataset_metadata(
        self,
        kind: CompiledReleaseSourceDatasetType,
        ids: &[Uuid],
    ) -> anyhow::Result<Vec<SourceDatasetReadIdentity>> {
        match self {
            Self::Database(pool) => fetch_source_dataset_metadata(pool, kind, ids).await,
            Self::Frozen(sources) => sources.dataset_metadata(kind, ids),
        }
    }

    pub(super) async fn exact_dataset_rows(
        self,
        kind: CompiledReleaseSourceDatasetType,
        identities: &[SourceDatasetReadIdentity],
    ) -> anyhow::Result<Vec<MethodRow>> {
        match self {
            Self::Database(pool) => fetch_exact_source_dataset_rows(pool, kind, identities).await,
            Self::Frozen(sources) => Ok(sources.exact_dataset_rows(kind, identities)),
        }
    }
}
