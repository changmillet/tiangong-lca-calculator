-- Add async TIDAS package job tables, queue, RPC, and RLS.
-- Additive-only migration.

BEGIN;

CREATE TABLE IF NOT EXISTS public.lca_package_jobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    job_type text NOT NULL,
    status text NOT NULL DEFAULT 'queued',
    payload jsonb NOT NULL DEFAULT '{}'::jsonb,
    diagnostics jsonb NOT NULL DEFAULT '{}'::jsonb,
    attempt integer NOT NULL DEFAULT 0,
    max_attempt integer NOT NULL DEFAULT 3,
    requested_by uuid NOT NULL,
    scope text,
    root_count integer NOT NULL DEFAULT 0,
    request_key text,
    idempotency_key text,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz,
    finished_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_package_jobs_type_chk
        CHECK (job_type IN ('export_package', 'import_package')),
    CONSTRAINT lca_package_jobs_status_chk
        CHECK (status IN ('queued', 'running', 'ready', 'completed', 'failed', 'stale')),
    CONSTRAINT lca_package_jobs_attempt_chk
        CHECK (attempt >= 0 AND max_attempt >= 0 AND attempt <= max_attempt),
    CONSTRAINT lca_package_jobs_root_count_chk
        CHECK (root_count >= 0),
    CONSTRAINT lca_package_jobs_request_key_chk
        CHECK (request_key IS NULL OR length(btrim(request_key)) > 0),
    CONSTRAINT lca_package_jobs_idempotency_key_chk
        CHECK (idempotency_key IS NULL OR length(btrim(idempotency_key)) > 0)
);

CREATE INDEX IF NOT EXISTS lca_package_jobs_status_created_idx
    ON public.lca_package_jobs (status, created_at);

CREATE INDEX IF NOT EXISTS lca_package_jobs_requested_by_created_idx
    ON public.lca_package_jobs (requested_by, created_at DESC);

CREATE INDEX IF NOT EXISTS lca_package_jobs_type_status_created_idx
    ON public.lca_package_jobs (job_type, status, created_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS lca_package_jobs_idempotency_key_uidx
    ON public.lca_package_jobs (idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS public.lca_package_artifacts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id uuid NOT NULL,
    artifact_kind text NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    artifact_url text NOT NULL,
    artifact_sha256 text,
    artifact_byte_size bigint,
    artifact_format text NOT NULL,
    content_type text NOT NULL,
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb,
    expires_at timestamptz,
    is_pinned boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_package_artifacts_job_fk
        FOREIGN KEY (job_id)
        REFERENCES public.lca_package_jobs (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_package_artifacts_kind_chk
        CHECK (artifact_kind IN (
            'import_source',
            'export_zip',
            'export_report',
            'import_report'
        )),
    CONSTRAINT lca_package_artifacts_status_chk
        CHECK (status IN ('pending', 'ready', 'failed', 'deleted')),
    CONSTRAINT lca_package_artifacts_format_chk
        CHECK (artifact_format IN (
            'tidas-package-zip:v1',
            'tidas-package-export-report:v1',
            'tidas-package-import-report:v1'
        )),
    CONSTRAINT lca_package_artifacts_size_chk
        CHECK (artifact_byte_size IS NULL OR artifact_byte_size >= 0),
    CONSTRAINT lca_package_artifacts_url_chk
        CHECK (length(btrim(artifact_url)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS lca_package_artifacts_job_kind_uidx
    ON public.lca_package_artifacts (job_id, artifact_kind);

CREATE INDEX IF NOT EXISTS lca_package_artifacts_job_created_idx
    ON public.lca_package_artifacts (job_id, created_at DESC);

CREATE INDEX IF NOT EXISTS lca_package_artifacts_status_created_idx
    ON public.lca_package_artifacts (status, created_at DESC);

CREATE TABLE IF NOT EXISTS public.lca_package_request_cache (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    requested_by uuid NOT NULL,
    operation text NOT NULL,
    request_key text NOT NULL,
    request_payload jsonb NOT NULL,
    status text NOT NULL DEFAULT 'pending',
    job_id uuid,
    export_artifact_id uuid,
    report_artifact_id uuid,
    error_code text,
    error_message text,
    hit_count bigint NOT NULL DEFAULT 0,
    last_accessed_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_package_request_cache_operation_chk
        CHECK (operation IN ('export_package', 'import_package')),
    CONSTRAINT lca_package_request_cache_status_chk
        CHECK (status IN ('pending', 'running', 'ready', 'failed', 'stale')),
    CONSTRAINT lca_package_request_cache_hit_count_chk
        CHECK (hit_count >= 0),
    CONSTRAINT lca_package_request_cache_request_key_chk
        CHECK (length(btrim(request_key)) > 0),
    CONSTRAINT lca_package_request_cache_job_fk
        FOREIGN KEY (job_id)
        REFERENCES public.lca_package_jobs (id)
        ON DELETE SET NULL,
    CONSTRAINT lca_package_request_cache_export_artifact_fk
        FOREIGN KEY (export_artifact_id)
        REFERENCES public.lca_package_artifacts (id)
        ON DELETE SET NULL,
    CONSTRAINT lca_package_request_cache_report_artifact_fk
        FOREIGN KEY (report_artifact_id)
        REFERENCES public.lca_package_artifacts (id)
        ON DELETE SET NULL,
    CONSTRAINT lca_package_request_cache_user_op_request_uk
        UNIQUE (requested_by, operation, request_key)
);

CREATE UNIQUE INDEX IF NOT EXISTS lca_package_request_cache_job_uidx
    ON public.lca_package_request_cache (job_id)
    WHERE job_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS lca_package_request_cache_lookup_idx
    ON public.lca_package_request_cache (requested_by, operation, status, updated_at DESC);

CREATE INDEX IF NOT EXISTS lca_package_request_cache_last_accessed_idx
    ON public.lca_package_request_cache (last_accessed_at DESC);

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pgmq.meta WHERE queue_name = 'lca_package_jobs'
    ) THEN
        PERFORM pgmq.create('lca_package_jobs');
    END IF;
END
$$;

CREATE OR REPLACE FUNCTION public.lca_package_enqueue_job(
    p_message jsonb
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pgmq
AS $$
DECLARE
    v_msg_id bigint;
BEGIN
    SELECT pgmq.send('lca_package_jobs', p_message)
      INTO v_msg_id;

    RETURN v_msg_id;
END;
$$;

REVOKE ALL ON FUNCTION public.lca_package_enqueue_job(jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.lca_package_enqueue_job(jsonb) FROM anon;
REVOKE ALL ON FUNCTION public.lca_package_enqueue_job(jsonb) FROM authenticated;
GRANT EXECUTE ON FUNCTION public.lca_package_enqueue_job(jsonb) TO service_role;

REVOKE ALL PRIVILEGES ON TABLE public.lca_package_jobs FROM anon, authenticated;
REVOKE ALL PRIVILEGES ON TABLE public.lca_package_artifacts FROM anon, authenticated;
REVOKE ALL PRIVILEGES ON TABLE public.lca_package_request_cache FROM anon, authenticated;

GRANT ALL PRIVILEGES ON TABLE public.lca_package_jobs TO service_role;
GRANT ALL PRIVILEGES ON TABLE public.lca_package_artifacts TO service_role;
GRANT ALL PRIVILEGES ON TABLE public.lca_package_request_cache TO service_role;

ALTER TABLE public.lca_package_jobs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.lca_package_artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.lca_package_request_cache ENABLE ROW LEVEL SECURITY;

GRANT SELECT ON TABLE public.lca_package_jobs TO authenticated;
DROP POLICY IF EXISTS lca_package_jobs_select_own ON public.lca_package_jobs;
CREATE POLICY lca_package_jobs_select_own
    ON public.lca_package_jobs
    FOR SELECT
    TO authenticated
    USING (requested_by = auth.uid());

GRANT SELECT ON TABLE public.lca_package_artifacts TO authenticated;
DROP POLICY IF EXISTS lca_package_artifacts_select_own ON public.lca_package_artifacts;
CREATE POLICY lca_package_artifacts_select_own
    ON public.lca_package_artifacts
    FOR SELECT
    TO authenticated
    USING (
        EXISTS (
            SELECT 1
            FROM public.lca_package_jobs AS j
            WHERE j.id = public.lca_package_artifacts.job_id
              AND j.requested_by = auth.uid()
        )
    );

GRANT SELECT ON TABLE public.lca_package_request_cache TO authenticated;
DROP POLICY IF EXISTS lca_package_request_cache_select_own
    ON public.lca_package_request_cache;
CREATE POLICY lca_package_request_cache_select_own
    ON public.lca_package_request_cache
    FOR SELECT
    TO authenticated
    USING (requested_by = auth.uid());

COMMIT;
