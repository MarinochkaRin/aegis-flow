# Activity + lease integration

**Status:** Domain-only implementation. The API models the intended rules; it is **not** a distributed lock.

The earlier domain model stored only `LeaseToken` in `Activity`, while a separate `Lease` had the deadline. That left a correctness gap: `Activity::finish` could accept a token after its lease expired. The two models now meet at the Activity API.

## Domain API

- `claim(lease, now)` rejects an already-expired lease and checks attempt overflow *before* state mutation.
- `heartbeat(token, now, duration)` renews only a running activity's current lease. An invalid heartbeat leaves the deadline unchanged.
- `finish(token, now, outcome)` checks both token and expiry, then changes state. An expired or stale worker cannot change the in-memory activity state.
- `recover_expired(now, policy)` operates only on an expired, running activity. Retry-safe work enters `RetryWaiting`; work with uncertain external effects enters `Ambiguous`. In both cases, the old lease is cleared.
- `scheduler_mark_ready()` models the transition from `RetryWaiting` to `Pending`. Only the **future durable scheduler** is supposed to call it after an actual backoff. The domain object does not verify database timers.
- `resolve_ambiguous()` remains the explicit application-driven reconciliation decision. The method itself cannot prove external results.

The API intentionally changes from `claim(LeaseToken)` to `claim(Lease, now)` and from `finish(&LeaseToken, outcome)` to `finish(&LeaseToken, now, outcome)`. Call sites in the old entity tests were updated accordingly.

## Failure walkthrough

1. The worker claims a broadcast activity with token A, deadline T+30s.
2. The RPC call is sent. The worker loses connectivity before it records whether the call succeeded.
3. After the deadline, `recover_expired` inspects the **previously declared, persisted** external-effect policy. For a transaction broadcast, that may require reconciliation. It must not blindly rebroadcast using a newly constructed transaction.
4. A stale worker reconnects with token A. It cannot complete the recovered activity. If a retry later receives token B, token A cannot complete the new attempt either.
5. Application-specific reconciliation checks the external result (for example, by transaction ID). Only after sufficient evidence may it report success, failure or safe-to-retry.

## What this code does *not* guarantee

- It accepts `now` from the caller. The PostgreSQL adapter must use one **authoritative database time** and enforce token, deadline and state checks together in a conditional transaction; independent workers must not supply their local wall-clock time as authority.
- `recover_expired` accepts an `ExternalEffectPolicy` for now. In production, the policy must be immutable per execution and read from durable storage; otherwise a caller could relabel unsafe work as retry-safe. This is an explicit storage-design requirement.
- No real queue, scheduler, worker, heartbeats or SQL transactions have been implemented yet.
- Lease fencing cannot stop a process already executing external side effects. Idempotency keys, external identifiers and reconciliation remain application responsibilities.

## Next milestone

Design the first PostgreSQL schema and claim/renew/finish transactions with `FOR UPDATE SKIP LOCKED`, a unique lease token, an expiry predicate, durable retry eligibility, and a persisted immutable effect policy. Test competing workers and lost responses against real PostgreSQL instead of relying only on in-memory domain tests.
