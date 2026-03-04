-- Drop legacy snapshot matrix tables for artifact-first architecture.
-- Safe scope: only remove obsolete lca_* matrix/index tables.

BEGIN;

DROP TABLE IF EXISTS public.lca_technosphere_entries CASCADE;
DROP TABLE IF EXISTS public.lca_biosphere_entries CASCADE;
DROP TABLE IF EXISTS public.lca_characterization_factors CASCADE;
DROP TABLE IF EXISTS public.lca_process_index CASCADE;
DROP TABLE IF EXISTS public.lca_flow_index CASCADE;

COMMIT;
