-- Aegis Flow v1: initial PostgreSQL storage contract.
-- Requires PostgreSQL 16. Applied ONCE to a new local database.
-- This milestone stores workflow/activity state, claims, lease fencing, and an audit trail.
-- It does not run external work or implement a production retry policy.
BEGIN;

CREATE SCHEMA aegis;

CREATE TABLE aegis.workflows (
    id UUID PRIMARY KEY,
    state TEXT NOT NULL DEFAULT 'Created'
        CHECK (state IN ('Created','Running','Waiting','Completed','Failed','Cancelled')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE TABLE aegis.activities (
    id UUID PRIMARY KEY,
    workflow_id UUID NOT NULL REFERENCES aegis.workflows(id),
    state TEXT NOT NULL DEFAULT 'Pending'
        CHECK (state IN ('Pending','Running','RetryWaiting','Succeeded','Failed','Ambiguous')),
    -- Persisted before first execution; changing it later would allow unsafe retries.
    effect_policy TEXT NOT NULL
        CHECK (effect_policy IN ('SafeToRetry','RequiresReconciliation')),
    available_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    attempt BIGINT NOT NULL DEFAULT 0 CHECK (attempt BETWEEN 0 AND 4294967295),
    worker_id TEXT,
    lease_token UUID,
    lease_until TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CONSTRAINT lease_fields_match_state CHECK (
        (state = 'Running' AND worker_id IS NOT NULL
             AND lease_token IS NOT NULL AND lease_until IS NOT NULL)
        OR
        (state <> 'Running' AND worker_id IS NULL
             AND lease_token IS NULL AND lease_until IS NULL)
    )
);

CREATE INDEX activities_due_idx ON aegis.activities (available_at, created_at, id)
    WHERE state = 'Pending';
CREATE INDEX activities_retry_idx ON aegis.activities (available_at, id)
    WHERE state = 'RetryWaiting';
CREATE INDEX activities_expired_idx ON aegis.activities (lease_until, id)
    WHERE state = 'Running';
CREATE INDEX activities_workflow_idx ON aegis.activities (workflow_id);

CREATE TABLE aegis.events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    workflow_id UUID NOT NULL REFERENCES aegis.workflows(id),
    activity_id UUID REFERENCES aegis.activities(id),
    event_type TEXT NOT NULL CHECK (length(btrim(event_type)) > 0),
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX events_workflow_idx ON aegis.events(workflow_id, id);
CREATE INDEX events_activity_idx ON aegis.events(activity_id, id)
    WHERE activity_id IS NOT NULL;

CREATE FUNCTION aegis.prevent_effect_policy_change()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Activity effect policy cannot be changed after creation'
        USING ERRCODE = 'P2001';
END;
$$;
CREATE TRIGGER activities_effect_policy_immutable
    BEFORE UPDATE OF effect_policy ON aegis.activities
    FOR EACH ROW
    WHEN (OLD.effect_policy IS DISTINCT FROM NEW.effect_policy)
    EXECUTE FUNCTION aegis.prevent_effect_policy_change();

CREATE FUNCTION aegis.prevent_event_mutation()
RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Event history is append-only'
        USING ERRCODE = 'P2002';
END;
$$;
CREATE TRIGGER events_append_only
    BEFORE UPDATE OR DELETE ON aegis.events
    FOR EACH ROW EXECUTE FUNCTION aegis.prevent_event_mutation();

-- One atomic claim. SKIP LOCKED ensures competing workers do not claim the same
-- Pending activity. Execution begins only after the calling transaction commits.
CREATE FUNCTION aegis.claim_activity(p_worker_id TEXT, p_lease_duration INTERVAL)
RETURNS TABLE (
    activity_id UUID,
    workflow_id UUID,
    attempt BIGINT,
    lease_token UUID,
    lease_until TIMESTAMPTZ
)
LANGUAGE plpgsql AS $$
BEGIN
    IF p_worker_id IS NULL OR length(btrim(p_worker_id)) = 0 THEN
        RAISE EXCEPTION 'worker ID must not be empty';
    END IF;
    IF p_lease_duration IS NULL OR p_lease_duration <= INTERVAL '0 seconds' THEN
        RAISE EXCEPTION 'lease duration must be positive';
    END IF;

    RETURN QUERY
    WITH db_time AS MATERIALIZED (
        SELECT clock_timestamp() AS now_at
    ), candidate AS (
        SELECT a.id
        FROM aegis.activities AS a
        JOIN aegis.workflows AS w ON w.id = a.workflow_id
        CROSS JOIN db_time AS t
        WHERE a.state = 'Pending'
          AND w.state = 'Running'
          AND a.available_at <= t.now_at
          AND a.attempt < 4294967295
        ORDER BY a.available_at, a.created_at, a.id
        FOR UPDATE OF a SKIP LOCKED
        LIMIT 1
    ), claimed AS (
        UPDATE aegis.activities AS a SET
            state = 'Running',
            attempt = a.attempt + 1,
            worker_id = p_worker_id,
            lease_token = gen_random_uuid(),
            lease_until = t.now_at + p_lease_duration,
            updated_at = t.now_at
        FROM candidate AS c CROSS JOIN db_time AS t
        WHERE a.id = c.id
        RETURNING a.id, a.workflow_id, a.attempt, a.lease_token, a.lease_until
    ), logged AS (
        INSERT INTO aegis.events(workflow_id, activity_id, event_type, details)
        SELECT c.workflow_id, c.id, 'ActivityClaimed',
               jsonb_build_object('attempt', c.attempt, 'worker_id', p_worker_id)
        FROM claimed AS c
        RETURNING activity_id
    )
    SELECT c.id, c.workflow_id, c.attempt, c.lease_token, c.lease_until
    FROM claimed AS c JOIN logged AS l ON l.activity_id = c.id;
END;
$$;

-- NULL means no row matched: wrong token, wrong state, or already expired.
-- A heartbeat cannot shorten the existing lease.
CREATE FUNCTION aegis.heartbeat_activity(
    p_activity_id UUID, p_lease_token UUID, p_duration INTERVAL
) RETURNS TIMESTAMPTZ LANGUAGE plpgsql AS $$
DECLARE renewed_until TIMESTAMPTZ;
BEGIN
    IF p_duration IS NULL OR p_duration <= INTERVAL '0 seconds' THEN
        RAISE EXCEPTION 'heartbeat duration must be positive';
    END IF;

    WITH db_time AS MATERIALIZED (SELECT clock_timestamp() AS now_at)
    UPDATE aegis.activities AS a SET
        lease_until = GREATEST(a.lease_until, t.now_at + p_duration),
        updated_at = t.now_at
    FROM db_time AS t
    WHERE a.id = p_activity_id AND a.state = 'Running'
      AND a.lease_token = p_lease_token AND a.lease_until > t.now_at
    RETURNING a.lease_until INTO renewed_until;
    RETURN renewed_until;
END;
$$;

-- Retry delay is supplied by a future scheduler (not implemented in Step 6A).
-- A stale or expired owner receives false and changes nothing.
CREATE FUNCTION aegis.finish_activity(
    p_activity_id UUID,
    p_lease_token UUID,
    p_outcome TEXT,
    p_next_available_at TIMESTAMPTZ DEFAULT NULL
) RETURNS BOOLEAN LANGUAGE plpgsql AS $$
DECLARE
    affected_workflow UUID;
    new_state TEXT;
BEGIN
    new_state := CASE p_outcome
        WHEN 'Success' THEN 'Succeeded'
        WHEN 'RetryableFailure' THEN 'RetryWaiting'
        WHEN 'PermanentFailure' THEN 'Failed'
        WHEN 'Ambiguous' THEN 'Ambiguous'
        ELSE NULL
    END;
    IF new_state IS NULL THEN
        RAISE EXCEPTION 'invalid activity outcome: %', p_outcome;
    END IF;
    IF new_state = 'RetryWaiting' AND p_next_available_at IS NULL THEN
        RAISE EXCEPTION 'retryable failure requires a durable next-available timestamp';
    END IF;

    WITH db_time AS MATERIALIZED (SELECT clock_timestamp() AS now_at)
    UPDATE aegis.activities AS a SET
        state = new_state,
        worker_id = NULL,
        lease_token = NULL,
        lease_until = NULL,
        available_at = CASE WHEN new_state = 'RetryWaiting'
                            THEN p_next_available_at ELSE a.available_at END,
        updated_at = t.now_at
    FROM db_time AS t
    WHERE a.id = p_activity_id
      AND a.state = 'Running'
      AND a.lease_token = p_lease_token
      AND a.lease_until > t.now_at
    RETURNING a.workflow_id INTO affected_workflow;

    IF NOT FOUND THEN
        RETURN false;
    END IF;
    INSERT INTO aegis.events(workflow_id, activity_id, event_type, details)
    VALUES (affected_workflow, p_activity_id, 'ActivityFinished',
            jsonb_build_object('outcome', p_outcome, 'new_state', new_state));
    RETURN true;
END;
$$;

-- Reclaim up to 100 expired rows without waiting on activities locked elsewhere.
-- Effect policy was persisted at creation and cannot be relabelled on recovery.
CREATE FUNCTION aegis.recover_expired(p_retry_delay INTERVAL DEFAULT INTERVAL '0 seconds')
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE recovered INTEGER;
BEGIN
    IF p_retry_delay IS NULL OR p_retry_delay < INTERVAL '0 seconds' THEN
        RAISE EXCEPTION 'recovery delay must be nonnegative';
    END IF;

    WITH db_time AS MATERIALIZED (SELECT clock_timestamp() AS now_at),
    victims AS (
        SELECT a.id
        FROM aegis.activities AS a CROSS JOIN db_time AS t
        WHERE a.state = 'Running' AND a.lease_until <= t.now_at
        ORDER BY a.lease_until, a.id
        FOR UPDATE OF a SKIP LOCKED
        LIMIT 100
    ), recovered_rows AS (
        UPDATE aegis.activities AS a SET
            state = CASE a.effect_policy
                       WHEN 'SafeToRetry' THEN 'RetryWaiting'
                       ELSE 'Ambiguous' END,
            available_at = CASE WHEN a.effect_policy = 'SafeToRetry'
                                THEN t.now_at + p_retry_delay ELSE a.available_at END,
            worker_id = NULL,
            lease_token = NULL,
            lease_until = NULL,
            updated_at = t.now_at
        FROM victims AS v CROSS JOIN db_time AS t
        WHERE a.id = v.id
        RETURNING a.id, a.workflow_id, a.state
    ), logged AS (
        INSERT INTO aegis.events(workflow_id, activity_id, event_type, details)
        SELECT r.workflow_id, r.id, 'ActivityLeaseExpired',
               jsonb_build_object('recovered_state', r.state)
        FROM recovered_rows AS r
        RETURNING activity_id
    )
    SELECT count(*)::INTEGER INTO recovered FROM logged;
    RETURN recovered;
END;
$$;

-- Make due retries eligible. `claim_activity` itself accepts only Pending work.
CREATE FUNCTION aegis.release_due_retries()
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE released INTEGER;
BEGIN
    WITH db_time AS MATERIALIZED (SELECT clock_timestamp() AS now_at),
    due AS (
        SELECT a.id
        FROM aegis.activities AS a CROSS JOIN db_time AS t
        WHERE a.state = 'RetryWaiting' AND a.available_at <= t.now_at
        ORDER BY a.available_at, a.id
        FOR UPDATE OF a SKIP LOCKED
        LIMIT 100
    ), changed AS (
        UPDATE aegis.activities AS a SET state = 'Pending', updated_at = t.now_at
        FROM due AS d CROSS JOIN db_time AS t
        WHERE a.id = d.id
        RETURNING a.id, a.workflow_id
    ), logged AS (
        INSERT INTO aegis.events(workflow_id, activity_id, event_type)
        SELECT c.workflow_id, c.id, 'ActivityRetryReleased' FROM changed AS c
        RETURNING activity_id
    )
    SELECT count(*)::INTEGER INTO released FROM logged;
    RETURN released;
END;
$$;

COMMIT;
