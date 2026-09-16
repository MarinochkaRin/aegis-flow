# Problem Statement

Long-running distributed operations cannot rely on the lifetime of a single process.

Aegis Flow treats failure scenarios as design inputs rather than edge cases.

## P-01 — Worker crash

### Scenario

A worker disappears while executing an activity.

### Expected system behaviour

The workflow must remain recoverable and the activity must eventually become eligible for safe reassignment.

### Open design question

How is ownership of in-flight work represented and expired?

## P-02 — Lost response

### Scenario

A remote side effect succeeds but the response never reaches the worker.

### Expected system behaviour

The system must not assume that timeout means failure.

### Open design question

How can the result be reconciled before retrying?

## P-03 — Duplicate execution

### Scenario

The same logical activity is delivered or claimed more than once.

### Expected system behaviour

Duplicate side effects must be prevented, detected, or reconciled.

### Open design question

Which operations require idempotency keys, deduplication, or state checks?

## P-04 — Provider outage

### Scenario

An external API or RPC provider becomes unavailable.

### Expected system behaviour

The workflow must remain durable while execution is delayed or rerouted.

### Open design question

Who owns provider failover and retry policy?

## P-05 — Rate limiting

### Scenario

A provider returns `429` or an equivalent throttling signal.

### Expected system behaviour

The system should back off without creating synchronized retry storms.

### Open design question

How are backoff and jitter represented?

## P-06 — Process restart

### Scenario

The workflow service itself restarts during active execution.

### Expected system behaviour

Authoritative workflow state must be reconstructed from durable storage.

### Open design question

What exactly must be persisted to resume correctly?

## P-07 — Long-running timer

### Scenario

A workflow needs to wait minutes, hours, or days.

### Expected system behaviour

The wait must survive process and machine restarts.

### Open design question

Are timers persisted as deadlines, events, tasks, or another durable representation?

## P-08 — Ambiguous side effect

### Scenario

The system cannot immediately determine whether an external side effect occurred.

### Expected system behaviour

Execution should move into a state that permits reconciliation instead of blind retry.

### Open design question

How should ambiguous outcomes be represented in the domain model?

## P-09 — Blockchain reorganization

### Scenario

A previously observed blockchain confirmation becomes invalid after a reorg.

### Expected system behaviour

The workflow must be able to revisit settlement state without violating previous invariants.

### Open design question

When is a transaction considered sufficiently final for a workflow to complete?

## Design implication

The execution model is not complete until it defines behaviour for each of these scenarios.

Happy-path correctness is insufficient.