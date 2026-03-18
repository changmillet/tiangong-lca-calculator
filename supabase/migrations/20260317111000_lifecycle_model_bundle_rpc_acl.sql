-- Restrict lifecycle model bundle RPC execution to service_role only.

BEGIN;

REVOKE EXECUTE ON FUNCTION public.save_lifecycle_model_bundle(jsonb) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION public.save_lifecycle_model_bundle(jsonb) FROM anon;
REVOKE EXECUTE ON FUNCTION public.save_lifecycle_model_bundle(jsonb) FROM authenticated;
GRANT EXECUTE ON FUNCTION public.save_lifecycle_model_bundle(jsonb) TO service_role;

REVOKE EXECUTE ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) FROM anon;
REVOKE EXECUTE ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) FROM authenticated;
GRANT EXECUTE ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) TO service_role;

COMMIT;
