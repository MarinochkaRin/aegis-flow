# Durable Workflow Engines — Research

## Purpose

This document compares existing durable workflow execution systems before Aegis Flow defines its own execution model.

The goal is not to reproduce another implementation.

The goal is to identify proven ideas, understand their trade-offs, and make Aegis Flow's own decisions explicit.

## Systems under review

- Temporal / Cadence
- Cloudflare Workflows
- AWS Step Functions
- Database-backed workflow and job engines

## Evaluation criteria

### Persistence

- What is the authoritative workflow state?
- Is state represented as mutable state, event history, or both?
- What happens after process restart?

### Execution semantics

- At-most-once, at-least-once, or another model?
- How are duplicates handled?
- What guarantees exist around side effects?

### Workers

- How is work assigned?
- What happens when a worker disappears?
- Are leases, acknowledgements, heartbeats, or another mechanism used?

### Timers

- How are long delays represented?
- Can timers survive process restarts?

### Retries

- Which failures are retryable?
- Who owns retry policy?
- How are backoff and jitter implemented?

### Determinism

- Does workflow code need to be deterministic?
- If so, why and what restrictions does that introduce?

### Failure recovery

How does the system recover after:

- worker crash;
- service restart;
- network partition;
- duplicated delivery?

### Operational complexity

- What infrastructure is required?
- How difficult is the system to operate?
- What are the major scaling constraints?

## Review format

For every system:

### Problem

What problem is the system trying to solve?

### Approach

How does it solve the problem?

### Strengths

What works particularly well?

### Trade-offs

What complexity or restrictions does the approach introduce?

### Ideas worth adopting

Which principles may fit Aegis Flow?

### Ideas deliberately rejected

Which approaches do not fit Aegis Flow, and why?

---

## Temporal / Cadence

Research pending.

## Cloudflare Workflows

Research pending.

## AWS Step Functions

Research pending.

## Database-backed approaches

Research pending.

## Aegis Flow conclusions

To be written after the individual reviews are complete.