-- Add snapshot artifact metadata for matrix-file-first workflow.
-- This migration is additive-only.

BEGIN;

CREATE TABLE IF NOT EXISTS public.lca_snapshot_artifacts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    snapshot_id uuid NOT NULL,
    artifact_url text NOT NULL,
    artifact_sha256 text NOT NULL,
    artifact_byte_size bigint NOT NULL,
    artifact_format text NOT NULL,
    process_count integer NOT NULL,
    flow_count integer NOT NULL,
    impact_count integer NOT NULL,
    a_nnz bigint NOT NULL,
    b_nnz bigint NOT NULL,
    c_nnz bigint NOT NULL,
    coverage jsonb,
    status text NOT NULL DEFAULT 'ready',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_snapshot_artifacts_snapshot_fk
        FOREIGN KEY (snapshot_id)
        REFERENCES public.lca_network_snapshots (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_snapshot_artifacts_status_chk
        CHECK (status IN ('ready', 'stale', 'failed')),
    CONSTRAINT lca_snapshot_artifacts_size_chk
        CHECK (artifact_byte_size >= 0),
    CONSTRAINT lca_snapshot_artifacts_counts_chk
        CHECK (
            process_count >= 0
            AND flow_count >= 0
            AND impact_count >= 0
            AND a_nnz >= 0
            AND b_nnz >= 0
            AND c_nnz >= 0
        )
);

CREATE UNIQUE INDEX IF NOT EXISTS lca_snapshot_artifacts_snapshot_format_key
    ON public.lca_snapshot_artifacts (snapshot_id, artifact_format);

CREATE INDEX IF NOT EXISTS lca_snapshot_artifacts_snapshot_status_idx
    ON public.lca_snapshot_artifacts (snapshot_id, status, created_at DESC);

CREATE INDEX IF NOT EXISTS lca_snapshot_artifacts_created_idx
    ON public.lca_snapshot_artifacts (created_at DESC);

COMMIT;
