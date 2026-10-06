-- This test runs inside one transaction and ROLLBACKs all test data.
-- Requires a fresh database already initialized by 0001_init.sql.
\set ON_ERROR_STOP on
BEGIN;
DO $$
DECLARE
    wf_id UUID := gen_random_uuid();
    activity_id UUID := gen_random_uuid();
    safe_id UUID := gen_random_uuid();
    sensitive_id UUID := gen_random_uuid();
    claim_result RECORD;
    retry_claim RECORD;
    saved_token UUID;
    changed BOOLEAN;
    heartbeat_at TIMESTAMPTZ;
    current_state TEXT;
    current_attempt BIGINT;
    n INTEGER;
BEGIN
    INSERT INTO aegis.workflows(id, state) VALUES (wf_id, 'Running');
    INSERT INTO aegis.activities(id, workflow_id, effect_policy)
    VALUES (activity_id, wf_id, 'SafeToRetry');

    SELECT * INTO claim_result FROM aegis.claim_activity('worker-1', INTERVAL '30 seconds');
    IF NOT FOUND OR claim_result.activity_id <> activity_id OR claim_result.attempt <> 1 THEN
        RAISE EXCEPTION 'initial claim did not assign attempt 1';
    END IF;
    saved_token := claim_result.lease_token;

    SELECT count(*) INTO n FROM aegis.claim_activity('worker-2', INTERVAL '30 seconds');
    IF n <> 0 THEN
        RAISE EXCEPTION 'a second worker claimed already-claimed activity';
    END IF;

    SELECT aegis.heartbeat_activity(activity_id, gen_random_uuid(), INTERVAL '30 seconds')
        INTO heartbeat_at;
    IF heartbeat_at IS NOT NULL THEN
        RAISE EXCEPTION 'heartbeat with stale token succeeded';
    END IF;
    SELECT aegis.heartbeat_activity(activity_id, saved_token, INTERVAL '60 seconds')
        INTO heartbeat_at;
    IF heartbeat_at IS NULL OR heartbeat_at <= claim_result.lease_until THEN
        RAISE EXCEPTION 'valid heartbeat did not extend lease';
    END IF;

    SELECT aegis.finish_activity(activity_id, gen_random_uuid(), 'Success') INTO changed;
    IF changed THEN
        RAISE EXCEPTION 'stale worker completed activity';
    END IF;
    SELECT aegis.finish_activity(activity_id, saved_token, 'Success') INTO changed;
    IF NOT changed THEN
        RAISE EXCEPTION 'current worker could not complete activity';
    END IF;
    SELECT state INTO current_state FROM aegis.activities WHERE id = activity_id;
    IF current_state <> 'Succeeded' THEN
        RAISE EXCEPTION 'successful completion did not persist';
    END IF;

    -- A lost response to an externally side-effecting operation is ambiguous.
    INSERT INTO aegis.activities(id, workflow_id, effect_policy)
    VALUES (sensitive_id, wf_id, 'RequiresReconciliation');
    SELECT * INTO claim_result FROM aegis.claim_activity('worker-3', INTERVAL '30 seconds');
    IF claim_result.activity_id <> sensitive_id THEN
        RAISE EXCEPTION 'sensitive activity was not claimed';
    END IF;
    saved_token := claim_result.lease_token;
    UPDATE aegis.activities SET lease_until = clock_timestamp() - INTERVAL '1 second'
    WHERE id = sensitive_id; -- simulate a dead worker (tests only)
    SELECT aegis.recover_expired(INTERVAL '0 seconds') INTO n;
    IF n <> 1 THEN
        RAISE EXCEPTION 'expected one expired activity to recover, got %', n;
    END IF;
    SELECT state INTO current_state FROM aegis.activities WHERE id = sensitive_id;
    IF current_state <> 'Ambiguous' THEN
        RAISE EXCEPTION 'unsafe work was retried without reconciliation';
    END IF;
    SELECT aegis.finish_activity(sensitive_id, saved_token, 'Success') INTO changed;
    IF changed THEN
        RAISE EXCEPTION 'expired token modified recovered activity';
    END IF;

    -- Safe work can enter retry waiting, then become eligible again.
    INSERT INTO aegis.activities(id, workflow_id, effect_policy)
    VALUES (safe_id, wf_id, 'SafeToRetry');
    SELECT * INTO claim_result FROM aegis.claim_activity('worker-4', INTERVAL '30 seconds');
    IF claim_result.activity_id <> safe_id THEN
        RAISE EXCEPTION 'retry-safe activity was not claimed';
    END IF;
    saved_token := claim_result.lease_token;
    UPDATE aegis.activities SET lease_until = clock_timestamp() - INTERVAL '1 second'
    WHERE id = safe_id;
    SELECT aegis.recover_expired(INTERVAL '0 seconds') INTO n;
    IF n <> 1 THEN
        RAISE EXCEPTION 'retry-safe recovery failed';
    END IF;
    SELECT state INTO current_state FROM aegis.activities WHERE id = safe_id;
    IF current_state <> 'RetryWaiting' THEN
        RAISE EXCEPTION 'retry-safe recovery selected incorrect state';
    END IF;
    SELECT aegis.release_due_retries() INTO n;
    IF n <> 1 THEN
        RAISE EXCEPTION 'retry scheduler failed to release one activity';
    END IF;
    SELECT * INTO retry_claim FROM aegis.claim_activity('worker-5', INTERVAL '30 seconds');
    IF retry_claim.activity_id <> safe_id OR retry_claim.attempt <> 2
       OR retry_claim.lease_token = saved_token THEN
        RAISE EXCEPTION 'retry did not receive a fresh token and attempt';
    END IF;
    SELECT aegis.finish_activity(safe_id, saved_token, 'Success') INTO changed;
    IF changed THEN
        RAISE EXCEPTION 'previous attempt completed new lease';
    END IF;

    -- The policy cannot be secretly changed to make an unsafe retry appear safe.
    BEGIN
        UPDATE aegis.activities SET effect_policy = 'SafeToRetry'
        WHERE id = sensitive_id;
        RAISE EXCEPTION 'effect policy was unexpectedly mutable';
    EXCEPTION WHEN SQLSTATE 'P2001' THEN
        NULL; -- expected trigger rejection
    END;

    SELECT count(*) INTO n FROM aegis.events WHERE workflow_id = wf_id;
    IF n < 7 THEN
        RAISE EXCEPTION 'audit events missing: found %', n;
    END IF;
    RAISE NOTICE 'PASS: claim, fencing, heartbeat, expiry policy, retry release, audit';
END;
$$;
ROLLBACK;
