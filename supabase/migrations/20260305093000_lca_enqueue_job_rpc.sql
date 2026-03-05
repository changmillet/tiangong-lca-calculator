-- Add queue enqueue RPC for Edge Functions to send pgmq jobs without direct postgres client.
-- Additive-only migration.

BEGIN;

CREATE OR REPLACE FUNCTION public.lca_enqueue_job(
    p_queue_name text,
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
    IF p_queue_name IS NULL OR btrim(p_queue_name) = '' THEN
        RAISE EXCEPTION 'queue name is required';
    END IF;

    SELECT pgmq.send(p_queue_name, p_message)
      INTO v_msg_id;

    RETURN v_msg_id;
END;
$$;

REVOKE ALL ON FUNCTION public.lca_enqueue_job(text, jsonb) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.lca_enqueue_job(text, jsonb) TO service_role;

COMMIT;
