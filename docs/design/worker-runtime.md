# Worker runtime

## Scope

This milestone introduces the first execution loop in Aegis Flow. The worker claims one activity from PostgreSQL, runs application-provided execution code, renews the lease while that code is running, and asks PostgreSQL to durably accept the final outcome.

The worker is not yet a daemon, scheduler, handler registry, or sandbox. `run_once` deliberately handles one activity so the ownership rules can be tested before adding a long-lived process loop.

## Ownership model

PostgreSQL remains authoritative for ownership. A `ClaimedActivity` contains a lease token, but possession of the token alone is insufficient: heartbeat and completion also require an unexpired database-side lease.

The worker uses a heartbeat interval strictly shorter than the lease duration. Each heartbeat asks PostgreSQL to extend ownership by the full lease duration. The worker never reconstructs lease validity from its local clock.

If a heartbeat is rejected, or if completion loses a race with lease expiry, the worker returns `LeaseLost`. That result means only that Aegis Flow no longer accepts this worker as the owner. It does **not** prove that an external side effect failed or was cancelled.

## External effects and ambiguity

The executor returns one of the domain `ActivityOutcome` values. An RPC timeout after submission, a lost response, or any other situation where execution cannot prove whether an external effect happened must be represented as `Ambiguous`, not as a retryable failure.

If a worker process dies or loses database connectivity while executing, its lease eventually expires. PostgreSQL recovery then uses the activity's immutable effect policy to choose RetryWaiting or Ambiguous. This is the safety boundary for work whose external side effects cannot be rolled back.

## Retry scheduling

The worker config contains a retry delay for `RetryableFailure`. The PostgreSQL adapter converts that duration into an interval but computes `next_available_at` using `clock_timestamp()` inside PostgreSQL. This avoids basing durable retry scheduling on the worker machine's wall clock.

## Security notes

- Lease tokens are fencing values, not authentication secrets.
- A stale worker cannot durably finish an activity through the storage contract.
- Losing a lease never authorizes an automatic assumption about the external system.
- The runtime currently executes trusted in-process Rust code. It is **not** a sandbox for untrusted user code.
- Database least-privilege roles, TLS, secret management, and worker process isolation are separate hardening milestones.

## Next milestone

The next worker milestone should add the long-lived process layer: idle polling, transient storage-error backoff, graceful shutdown, periodic retry release/recovery, structured tracing, and a real handler registry keyed by activity type/payload.
