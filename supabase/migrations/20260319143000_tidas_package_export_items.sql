-- Add durable export item state for resumable TIDAS package exports.
-- Additive-only migration.

BEGIN;

CREATE TABLE IF NOT EXISTS public.lca_package_export_items (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id uuid NOT NULL,
    table_name text NOT NULL,
    dataset_id uuid NOT NULL,
    version text NOT NULL,
    is_seed boolean NOT NULL DEFAULT false,
    refs_done boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT lca_package_export_items_job_fk
        FOREIGN KEY (job_id)
        REFERENCES public.lca_package_jobs (id)
        ON DELETE CASCADE,
    CONSTRAINT lca_package_export_items_table_chk
        CHECK (table_name IN (
            'contacts',
            'sources',
            'unitgroups',
            'flowproperties',
            'flows',
            'processes',
            'lifecyclemodels'
        )),
    CONSTRAINT lca_package_export_items_version_chk
        CHECK (length(btrim(version)) > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS lca_package_export_items_job_dataset_uidx
    ON public.lca_package_export_items (job_id, table_name, dataset_id, version);

CREATE INDEX IF NOT EXISTS lca_package_export_items_job_refs_idx
    ON public.lca_package_export_items (job_id, refs_done, created_at, table_name);

CREATE INDEX IF NOT EXISTS lca_package_export_items_job_seed_idx
    ON public.lca_package_export_items (job_id, is_seed, created_at);

REVOKE ALL PRIVILEGES ON TABLE public.lca_package_export_items FROM anon, authenticated;
GRANT ALL PRIVILEGES ON TABLE public.lca_package_export_items TO service_role;

ALTER TABLE public.lca_package_export_items ENABLE ROW LEVEL SECURITY;

COMMIT;
