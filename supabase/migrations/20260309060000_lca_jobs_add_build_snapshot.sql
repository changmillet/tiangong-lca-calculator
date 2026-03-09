-- Extend lca_jobs job_type constraint to support build_snapshot.

BEGIN;

ALTER TABLE public.lca_jobs
    DROP CONSTRAINT IF EXISTS lca_jobs_type_chk;

ALTER TABLE public.lca_jobs
    ADD CONSTRAINT lca_jobs_type_chk
    CHECK (job_type IN (
        'prepare_factorization',
        'solve_one',
        'solve_batch',
        'solve_all_unit',
        'invalidate_factorization',
        'rebuild_factorization',
        'build_snapshot'
    ));

COMMIT;
