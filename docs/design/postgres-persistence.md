# PostgreSQL storage boundary — Step 6A

**Status:** first executable storage prototype; not a production-ready workflow engine.

After the in-memory domain model, the first persistence contract needs to enforce the invariants **inside PostgreSQL**, not only in Rust. The server's clock is authoritative for lease expiry; worker wall clocks are not.

## Initial storage model

- `aegis.workflows`: current state of a workflow. Terminal transition rules remain a domain concern and must also be guarded in the future storage adapter. Direct application SQL writes are **not** the intended API.
- `aegis.activities`: an activity's state, its parent workflow, persisted external-effect policy, durable eligibility timestamp, attempts, and active lease fields. Running activities have a complete lease; non-running activities have none.
- `aegis.events`: append-only audit events. These are not Temporal-style deterministic replay history.

All UUIDs line up with the public `flow-domain` newtypes. `attempt=0` in storage means never attempted; it becomes `Attempt::new(1)` on the first claim. Activity policy strings match `ExternalEffectPolicy::{SafeToRetry, RequiresReconciliation}`.

## Operations in this milestone

`aegis.claim_activity(worker, duration)` atomically selects one due Pending activity with a Running parent workflow using `FOR UPDATE OF a SKIP LOCKED`. It assigns a fresh UUID lease token, increments attempt, changes to Running and records an event. An empty result is **not** an error: no claimable work is available. Execution must begin **after** the transaction commits, never while holding the row lock.

`aegis.heartbeat_activity(id, token, duration)` renews a live lease without shortening it; a stale token or expired lease gets NULL.

`aegis.finish_activity(id, token, outcome, next_available_at)` accepts results only while the matching lease is live. It returns false for a stale/expired worker and writes an event after a successful state change. The caller must supply a durable timestamp when scheduling RetryableFailure; the retry-policy calculator is **not** implemented yet.

`aegis.recover_expired(delay)` processes up to 100 expired rows per call, using each activity's originally persisted effect policy: safe work becomes RetryWaiting, side-effecting work becomes Ambiguous. Expiry is never evidence that an external effect failed.

`aegis.release_due_retries()` changes due RetryWaiting rows to Pending. It is an operation for a future scheduler to invoke; this milestone has **no** running scheduler.

## Boundaries and unresolved work

1. These SQL functions are a small executable contract for storage behavior, **not** a substitute for the next Rust `flow-storage-pg` adapter. Tests must eventually use real overlapping worker transactions to establish concurrency guarantees in practice.
2. This version does not store Activity payloads, idempotency keys, executable code, error details, max attempts, exponential backoff, or workflow-stage orchestration. Each requires a deliberate schema/API design before real payment workloads are supported.
3. `finish_activity` accepts a retryable result and a supplied timestamp; the future scheduler must enforce retry policy and attempt limits.
4. `resolve_ambiguous` is deliberately absent here. It should require application-supplied reconciliation evidence with an auditable transition; neither lease expiry nor a simple timeout is proof of external failure.
5. The events trigger rejects UPDATE/DELETE for ordinary use, but a production deployment also needs separate database ownership, least-privilege roles, migrations, and operational backup/recovery policies.
6. SQL functions execute within the caller's transaction. Any claim transaction must be short and committed **before** any external work. Since DB time is sampled at statement execution, callers must not start work after an unbounded delay between claim and commit.
7. `clock_timestamp()` is read once per SQL statement in each operation for deadline comparisons and changes; external services must not independently decide lease validity.

## Local tests

`db/tests/smoke.sql` uses a transaction that rolls back its sample rows after checking single-worker claim exclusivity, fencing of stale tokens, heartbeats, expiry-to-Ambiguous, retry-safe recovery, new attempt/token on retry, immutable effect policy and audit events. This is a **smoke test**, not yet a concurrent multi-process test.

See `INSTALL.md` for the local Docker commands. The PostgreSQL dev container is bound to localhost:55432 to avoid colliding with an existing PostgreSQL service.
