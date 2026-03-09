-- Pointer table for latest solve_all_unit query artifacts per snapshot.
-- Additive-only migration.

BEGIN;

CREATE TABLE IF NOT EXISTS public.lca_latest_all_unit_results (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    snapshot_id uuid NOT NULL,
    job_id uuid NOT NULL,
    result_id uuid NOT NULL,
    query_artifact_url text NOT NULL,
    query_artifact_sha256 text NOT NULL,
    query_artifact_byte_size bigint NOT NULL,
    query_artifact_format text NOT NULL,
    status text NOT NULL DEFAULT 'ready',
    computed_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_latest_all_unit_results_snapshot_fk
        FOREIGN KEY (snapshot_id)
        REFERENCES public.lca_network_snapshots (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_latest_all_unit_results_job_fk
        FOREIGN KEY (job_id)
        REFERENCES public.lca_jobs (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_latest_all_unit_results_result_fk
        FOREIGN KEY (result_id)
        REFERENCES public.lca_results (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_latest_all_unit_results_snapshot_uk
        UNIQUE (snapshot_id),
    CONSTRAINT lca_latest_all_unit_results_status_chk
        CHECK (status IN ('ready', 'stale', 'failed')),
    CONSTRAINT lca_latest_all_unit_results_size_chk
        CHECK (query_artifact_byte_size >= 0)
);

CREATE INDEX IF NOT EXISTS lca_latest_all_unit_results_computed_idx
    ON public.lca_latest_all_unit_results (computed_at DESC);

CREATE INDEX IF NOT EXISTS lca_latest_all_unit_results_result_idx
    ON public.lca_latest_all_unit_results (result_id);

ALTER TABLE public.lca_latest_all_unit_results ENABLE ROW LEVEL SECURITY;

REVOKE ALL PRIVILEGES ON TABLE public.lca_latest_all_unit_results FROM anon, authenticated;
GRANT ALL PRIVILEGES ON TABLE public.lca_latest_all_unit_results TO service_role;

COMMIT;
