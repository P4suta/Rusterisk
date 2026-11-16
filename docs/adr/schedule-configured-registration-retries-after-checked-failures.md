---
date: 2026-11-16
status: proposed
---

# Schedule configured registration retries after checked failures

## Context

The [reference driver](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip_outbound_registration.c#L1465-L1495) distinguishes response instructions from configured retries.
[RFC 3261](https://www.rfc-editor.org/rfc/rfc3261.html#section-10.2.7) discourages immediate registration retries after timeouts.

## Decision

Use a pure policy returning stop or one scheduled attempt.
Positive Retry-After values take precedence without consuming or requiring configured retry budget.
Configured retries use fixed intervals and stop at `max_retries`.
Classify 408, 500, 502, 503, 504, and 6xx as temporary failures.
Classify 401 and 407 as temporary when `auth_rejection_permanent=no`.
Apply `forbidden_retry_interval` and `fatal_retry_interval` through the reference precedence.
Reset counters after valid successful grants.
Use checked clock arithmetic.
Waiting registrations don't block another address of record until dispatch.
Cancellation remains absorbing.

## Alternatives

Counting instructions in the budget bounds repeated hints but changes reference behavior.
Exponential delays change configured intervals.

## Consequences

Repeated hints can continue until configuration cancellation.
Client capacity and one pending attempt bound memory.
Alternate servers and shared 503 suppression remain incomplete.
Reconsider retry precedence only with an explicit compatibility change.

## Confirmation

Pure tables and native peers must cover precedence, exhaustion, counter reset, clock overflow, independent progress, and cancellation.
