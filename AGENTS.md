# Rusterisk development

Use the approved Asterisk 22.9.0 compatibility contract.
Keep each commit small enough to review as one behavior or development rule.
Write public artifacts in English and replies in Japanese.
Use Conventional Commits.

Use Red, Green, Refactor for behavior, adapters, settings, clients, examples, and verification tools.
Observe the intended test failure before adding production behavior.
Use `mise x -- just check` for formatting, the source policy, strict Clippy, types, and workspace tests.
Use `mise x -- just security` for dependency, license, and secret checks.
The fast gate checks the locked JavaScript fixture's types, source rules, formatting, and tests.
Use `mise x -- just node-check` to run those checks directly.
Inspect the diff, affected workflows, and maintained commands before completion and every push.
Run applicable checks and repair failures before reporting success.
Keep integration and release checks at their declared stages.

Keep call transitions, ownership, cancellation, and terminal cleanup in the dependency-free `no_std` core.
Use Rust 1.89-compatible language features in that crate and Rust 1.99.0 for development.
The fast gate runs the core tests with the pinned Rust 1.89.0 toolchain.
Use `mise x -- just core-msrv` to run the Rust 1.89.0 core tests directly.
Keep network, clock, filesystem, and persistence effects in adapters.
Serialize control decisions while continuing to receive signaling and media events.
Use bounded channels and fixed-capacity audio frames.
Every task needs an explicit lifetime and cleanup path.

Use concrete types, generics, exhaustive enums, moved ownership, and borrows.
Reject dynamic dispatch, `Box`, `Arc`, `Rc`, `RefCell`, and shared domain locks in project code.
Never substitute a single-element vector for heap indirection.
Reject unbounded queues, recoverable panics, ignored failures, and locks held across awaits.
Project code must remain free of `unsafe`.
The syntax gate covers all workspace production code and tests.
Clippy checks resolved types through aliases and wrappers.
Register necessary lint expectations in `api/policy-exceptions.toml` with the exact item, reason, and architecture decision.
Keep each expectation local and deny unused expectations.

Treat the public API types and operation contract as the source for generated definitions and the Rust client.
Distinguish accepted control requests, Stasis entry, peer answers, and connected media.
Declare call compatibility only after reference and received-audio tests pass.
Return an identifiable unsupported response for known operations without working adapters.
Keep authentication identity separate from displayed caller identification.
Keep credentials and audio content out of normal logs.

Reject unsupported settings with a file, line, and reason.
Validate complete replacement settings before reload.
Preserve a call's starting settings and require restart for listener changes.
Recover interrupted recordings and mark pre-restart calls interrupted.

Read the relevant primary specifications before changing protocol or interface behavior.
Use concise architecture decisions for consequential choices and keep unresolved decisions proposed.
Use the shared prose and architecture gates for authored documents.
Keep one sentence per line, avoid authored lists and tables, and preserve exact API identifiers.
Commit only when requested, and request separate authorization for pushes and publication.
