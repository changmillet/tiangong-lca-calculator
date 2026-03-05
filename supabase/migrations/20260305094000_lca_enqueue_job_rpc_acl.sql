-- Restrict lca_enqueue_job RPC execution to service_role only.
-- Additive-only security hardening migration.

BEGIN;

REVOKE EXECUTE ON FUNCTION public.lca_enqueue_job(text, jsonb) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION public.lca_enqueue_job(text, jsonb) FROM anon;
REVOKE EXECUTE ON FUNCTION public.lca_enqueue_job(text, jsonb) FROM authenticated;
GRANT EXECUTE ON FUNCTION public.lca_enqueue_job(text, jsonb) TO service_role;

COMMIT;
