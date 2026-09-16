# Aegis Flow

**A durable workflow execution engine written in Rust for operations that must survive crashes, retries, timeouts, and ambiguous remote results.**

## Why

Distributed systems become difficult when the system no longer knows whether an operation actually happened.

```text
Worker -> RPC -> transaction broadcast
                 |
                 v
            response lost
                 |
                 v
          Worker sees TIMEOUT
```

Did the operation fail? Did it succeed? Is retrying safe?

Aegis Flow explores how durable state, explicit state machines, idempotency, and Rust's type system can make these situations easier to reason about.

The first real-world use case is **blockchain transaction execution**, while the workflow engine itself remains domain-independent.

## Engineering approach

```text
Research
   -> Compare production approaches
   -> Define guarantees
   -> Architecture
   -> Implementation
   -> Failure testing
```

Important design decisions are documented before implementation and compared with established production approaches such as Temporal/Cadence, Cloudflare Workflows, AWS Step Functions, and database-backed execution engines.

## Current status

**Research & Architecture**

- [x] Problem definition
- [x] Engineering principles
- [ ] Workflow engine audit
- [ ] Execution model
- [ ] State machine
- [ ] Rust domain layer

## Documentation

- [Vision](docs/product/vision.md)
- [Problem](docs/product/problem.md)
- [Engineering Principles](docs/product/principles.md)
- [Workflow Engine Research](docs/research/workflow-engines.md)