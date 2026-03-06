-- Destructive migration:
--   * move solve results to S3-only persistence
--   * remove inline JSON payload storage from lca_results
--   * add retention metadata for GC
--   * clear legacy rows before tightening constraints

BEGIN;

-- Clear request/result cache first to avoid carrying stale references.
DELETE FROM public.lca_result_cache;

-- Legacy rows may have inline payload / missing artifact metadata.
DELETE FROM public.lca_results;

ALTER TABLE public.lca_results
    DROP COLUMN IF EXISTS payload;

ALTER TABLE public.lca_results
    ALTER COLUMN artifact_url SET NOT NULL,
    ALTER COLUMN artifact_sha256 SET NOT NULL,
    ALTER COLUMN artifact_byte_size SET NOT NULL,
    ALTER COLUMN artifact_format SET NOT NULL;

ALTER TABLE public.lca_results
    ADD COLUMN IF NOT EXISTS expires_at timestamptz NOT NULL DEFAULT (now() + interval '30 days'),
    ADD COLUMN IF NOT EXISTS is_pinned boolean NOT NULL DEFAULT false;

ALTER TABLE public.lca_results
    DROP CONSTRAINT IF EXISTS lca_results_artifact_format_chk;

ALTER TABLE public.lca_results
    ADD CONSTRAINT lca_results_artifact_format_chk
        CHECK (artifact_format IN ('hdf5:v1'));

CREATE UNIQUE INDEX IF NOT EXISTS lca_results_job_uidx
    ON public.lca_results (job_id);

CREATE INDEX IF NOT EXISTS lca_results_expires_at_idx
    ON public.lca_results (expires_at, created_at)
    WHERE is_pinned = false;

CREATE INDEX IF NOT EXISTS lca_results_created_desc_idx
    ON public.lca_results (created_at DESC);

COMMIT;
