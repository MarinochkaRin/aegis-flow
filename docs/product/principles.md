# Engineering Principles

## 1. Durable state outlives processes

A worker is disposable.

Losing a worker must not mean losing a workflow.

## 2. Guarantees must be explicit

Every subsystem should document what it guarantees and what it deliberately does not guarantee.

## 3. Duplicate execution is expected

Distributed work may execute more than once.

Side effects must therefore be idempotent, deduplicated, or explicitly reconciled.

## 4. Failure behaviour is part of the design

Timeouts, retries, crashes, and ambiguous results are normal states of a distributed system, not afterthoughts.

## 5. The source of truth must be explicit

Caches, queues, and indexes must not silently become authoritative state.

## 6. Domain logic should not depend on infrastructure

Core workflow rules should be testable without PostgreSQL, HTTP, Redis, or an async runtime.

## 7. Every distributed operation has identity

Stable identifiers are required for deduplication, tracing, recovery, correlation, and debugging.

## 8. Observability is part of correctness

Important operations must expose enough context to reconstruct what happened and why.

## 9. Architectural decisions require evidence

Important choices should be based on prior-art research, experiments, measurements, or clearly stated assumptions.

## 10. Documentation evolves with behaviour

A change that makes product, architecture, or reliability documentation incorrect is not complete.

## How these principles are used

These principles are not marketing statements.

They are intended to constrain architecture and code reviews.

When a design choice conflicts with one of them, the conflict should be documented explicitly.