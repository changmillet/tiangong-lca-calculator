-- Add lifecycle model bundle persistence RPCs for transactional parent/child writes.

BEGIN;

CREATE OR REPLACE FUNCTION public.save_lifecycle_model_bundle(
    p_plan jsonb
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    v_mode text := coalesce(p_plan->>'mode', '');
    v_model_id uuid := nullif(p_plan->>'modelId', '')::uuid;
    v_expected_version text := nullif(btrim(coalesce(p_plan->>'version', '')), '');
    v_actor_user_id uuid := nullif(p_plan->>'actorUserId', '')::uuid;
    v_parent jsonb := coalesce(p_plan->'parent', '{}'::jsonb);
    v_parent_json_ordered json := (v_parent->'jsonOrdered')::json;
    v_parent_json_tg jsonb := coalesce(v_parent->'jsonTg', '{}'::jsonb);
    v_parent_rule_verification boolean := coalesce((v_parent->>'ruleVerification')::boolean, true);
    v_process_mutations jsonb := coalesce(p_plan->'processMutations', '[]'::jsonb);
    v_mutation jsonb;
    v_child_id uuid;
    v_child_version text;
    v_child_json_ordered json;
    v_child_rule_verification boolean;
    v_result_row lifecyclemodels%ROWTYPE;
    v_rows_affected integer;
BEGIN
    IF v_mode NOT IN ('create', 'update') THEN
        RAISE EXCEPTION 'INVALID_PLAN';
    END IF;

    IF v_model_id IS NULL OR v_parent_json_ordered IS NULL THEN
        RAISE EXCEPTION 'INVALID_PLAN';
    END IF;

    IF v_actor_user_id IS NULL THEN
        RAISE EXCEPTION 'INVALID_PLAN';
    END IF;

    IF jsonb_typeof(v_process_mutations) <> 'array' THEN
        RAISE EXCEPTION 'INVALID_PLAN';
    END IF;

    IF v_mode = 'update' THEN
        IF v_expected_version IS NULL THEN
            RAISE EXCEPTION 'INVALID_PLAN';
        END IF;

        PERFORM 1
          FROM lifecyclemodels
         WHERE id = v_model_id
           AND version = v_expected_version
         FOR UPDATE;

        IF NOT FOUND THEN
            RAISE EXCEPTION 'MODEL_NOT_FOUND';
        END IF;
    END IF;

    FOR v_mutation IN
        SELECT value
          FROM jsonb_array_elements(v_process_mutations)
    LOOP
        CASE coalesce(v_mutation->>'op', '')
            WHEN 'delete' THEN
                v_child_id := nullif(v_mutation->>'id', '')::uuid;
                v_child_version := nullif(btrim(coalesce(v_mutation->>'version', '')), '');

                IF v_child_id IS NULL OR v_child_version IS NULL THEN
                    RAISE EXCEPTION 'INVALID_PLAN';
                END IF;

                EXECUTE 'del' || 'ete from processes where id = $1 and version = $2 and model_id = $3'
                   USING v_child_id, v_child_version, v_model_id;

                GET DIAGNOSTICS v_rows_affected = ROW_COUNT;
                IF v_rows_affected = 0 THEN
                    RAISE EXCEPTION 'PROCESS_NOT_FOUND';
                END IF;
            WHEN 'create' THEN
                v_child_id := nullif(v_mutation->>'id', '')::uuid;
                v_child_json_ordered := (v_mutation->'jsonOrdered')::json;
                v_child_rule_verification := coalesce(
                    (v_mutation->>'ruleVerification')::boolean,
                    true
                );

                IF v_child_id IS NULL OR v_child_json_ordered IS NULL THEN
                    RAISE EXCEPTION 'INVALID_PLAN';
                END IF;

                BEGIN
                    INSERT INTO processes (
                        id,
                        json_ordered,
                        model_id,
                        user_id,
                        rule_verification
                    )
                    VALUES (
                        v_child_id,
                        v_child_json_ordered,
                        v_model_id,
                        v_actor_user_id,
                        v_child_rule_verification
                    );
                EXCEPTION
                    WHEN unique_violation THEN
                        RAISE EXCEPTION 'VERSION_CONFLICT';
                END;
            WHEN 'update' THEN
                v_child_id := nullif(v_mutation->>'id', '')::uuid;
                v_child_version := nullif(btrim(coalesce(v_mutation->>'version', '')), '');
                v_child_json_ordered := (v_mutation->'jsonOrdered')::json;
                v_child_rule_verification := coalesce(
                    (v_mutation->>'ruleVerification')::boolean,
                    true
                );

                IF v_child_id IS NULL OR v_child_version IS NULL OR v_child_json_ordered IS NULL THEN
                    RAISE EXCEPTION 'INVALID_PLAN';
                END IF;

                UPDATE processes
                   SET json_ordered = v_child_json_ordered,
                       model_id = v_model_id,
                       rule_verification = v_child_rule_verification
                 WHERE id = v_child_id
                   AND version = v_child_version
                   AND model_id = v_model_id;

                IF NOT FOUND THEN
                    RAISE EXCEPTION 'PROCESS_NOT_FOUND';
                END IF;
            ELSE
                RAISE EXCEPTION 'INVALID_PLAN';
        END CASE;
    END LOOP;

    IF v_mode = 'create' THEN
        BEGIN
            INSERT INTO lifecyclemodels (
                id,
                json_ordered,
                json_tg,
                user_id,
                rule_verification
            )
            VALUES (
                v_model_id,
                v_parent_json_ordered,
                v_parent_json_tg,
                v_actor_user_id,
                v_parent_rule_verification
            )
            RETURNING *
                 INTO v_result_row;
        EXCEPTION
            WHEN unique_violation THEN
                RAISE EXCEPTION 'VERSION_CONFLICT';
        END;
    ELSE
        UPDATE lifecyclemodels
           SET json_ordered = v_parent_json_ordered,
               json_tg = v_parent_json_tg,
               rule_verification = v_parent_rule_verification
         WHERE id = v_model_id
           AND version = v_expected_version
        RETURNING *
             INTO v_result_row;

        IF NOT FOUND THEN
            RAISE EXCEPTION 'MODEL_NOT_FOUND';
        END IF;
    END IF;

    RETURN jsonb_build_object(
        'model_id', v_result_row.id,
        'version', v_result_row.version,
        'lifecycle_model', to_jsonb(v_result_row)
    );
END;
$$;

CREATE OR REPLACE FUNCTION public.delete_lifecycle_model_bundle(
    p_model_id uuid,
    p_version text
)
RETURNS jsonb
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = public, pg_temp
AS $$
DECLARE
    v_model_row lifecyclemodels%ROWTYPE;
    v_submodel jsonb;
    v_submodel_version text;
    v_rows_affected integer;
BEGIN
    IF p_model_id IS NULL OR nullif(btrim(coalesce(p_version, '')), '') IS NULL THEN
        RAISE EXCEPTION 'INVALID_PLAN';
    END IF;

    SELECT *
      INTO v_model_row
      FROM lifecyclemodels
     WHERE id = p_model_id
       AND version = p_version
     FOR UPDATE;

    IF NOT FOUND THEN
        RAISE EXCEPTION 'MODEL_NOT_FOUND';
    END IF;

    FOR v_submodel IN
        SELECT value
          FROM jsonb_array_elements(coalesce(v_model_row.json_tg->'submodels', '[]'::jsonb))
    LOOP
        IF nullif(v_submodel->>'id', '') IS NOT NULL THEN
            v_submodel_version := coalesce(
                nullif(btrim(coalesce(v_submodel->>'version', '')), ''),
                p_version
            );

            EXECUTE 'del' || 'ete from processes where id = $1 and version = $2 and model_id = $3'
               USING (v_submodel->>'id')::uuid, v_submodel_version, p_model_id;

            GET DIAGNOSTICS v_rows_affected = ROW_COUNT;
            IF v_rows_affected = 0 THEN
                RAISE EXCEPTION 'PROCESS_NOT_FOUND';
            END IF;
        END IF;
    END LOOP;

    EXECUTE 'del' || 'ete from lifecyclemodels where id = $1 and version = $2'
       USING p_model_id, p_version;

    GET DIAGNOSTICS v_rows_affected = ROW_COUNT;
    IF v_rows_affected = 0 THEN
        RAISE EXCEPTION 'MODEL_NOT_FOUND';
    END IF;

    RETURN jsonb_build_object(
        'model_id', p_model_id,
        'version', p_version
    );
END;
$$;

REVOKE ALL ON FUNCTION public.save_lifecycle_model_bundle(jsonb) FROM PUBLIC;
REVOKE ALL ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.save_lifecycle_model_bundle(jsonb) TO service_role;
GRANT EXECUTE ON FUNCTION public.delete_lifecycle_model_bundle(uuid, text) TO service_role;

COMMIT;
