-- Synthetic fixture for the worker-owned transport contract, not a migration.
CREATE SCHEMA private;
CREATE TABLE private.worker_jobs (
    id uuid PRIMARY KEY,
    status text NOT NULL DEFAULT 'queued',
    attempt_count bigint NOT NULL DEFAULT 0,
    lease_token uuid,
    lease_expires_at timestamptz
);
CREATE FUNCTION private.worker_claim_jobs(text, text, integer, integer) RETURNS jsonb
LANGUAGE sql AS $$
    WITH candidate AS (
        SELECT id FROM private.worker_jobs WHERE status='queued'
        ORDER BY id LIMIT $3 FOR UPDATE SKIP LOCKED
    ), claimed AS (
        UPDATE private.worker_jobs j SET status='running',
            attempt_count=j.attempt_count+1, lease_token=gen_random_uuid(),
            lease_expires_at=now()+make_interval(secs=>$4)
        FROM candidate c WHERE j.id=c.id RETURNING j.*
    ) SELECT jsonb_build_object('ok', true, 'data', coalesce(jsonb_agg(
        jsonb_build_object('id', id, 'jobKind', 'tidas.export', 'workerQueue', $1,
            'payloadSchemaVersion', 'test.v1', 'payload', '{}'::jsonb,
            'leaseToken', lease_token, 'attemptCount', attempt_count)
    ), '[]'::jsonb)) FROM claimed;
$$;
