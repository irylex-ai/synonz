# Testing Governance Rule

Operational rule for how Synonz verifies correctness, reliability, behavioral
stability, regression prevention, and long-term maintainability. This is a
specialized workflow under `AGENTS.md`; where conflict appears, `AGENTS.md`
takes precedence. This rule guides the Coding Agent, Review Agent, test-related
workflows, and human contributors.

This rule defines testing governance principles, not implementation testing
details. It does not specify test cases, module-level testing strategies, or
concrete testing frameworks. Those decisions are made within implementation
work, guided by the principles here.

## 1. Purpose

Testing exists to provide confidence in software behavior. The goal of testing
is not maximizing test count, achieving arbitrary coverage numbers, or creating
tests only for metrics. The goal is to:

- verify meaningful behavior;
- prevent regressions;
- validate important system guarantees;
- increase confidence during evolution.

Prefer meaningful tests over excessive tests, behavior verification over
implementation verification, and stable tests over fragile tests.

## 2. Testing Principles

- Tests should validate expected behavior.
- Tests should protect important guarantees.
- Tests should be maintainable.
- Tests should provide confidence for future changes.

High test coverage does not automatically mean high quality. Coverage measures
which code was executed, not whether meaningful behavior was verified. Tests
that exercise code without asserting behavior add cost without adding
confidence. Tests are valuable when they protect behavior that matters.

## 3. Test Pyramid

Each testing level serves a different purpose. Use the lowest-cost level that
provides sufficient confidence, proportional to the risk and scope of the
change.

### Unit Tests

Purpose:

- validate isolated logic;
- verify small components;
- provide fast feedback.

Characteristics:

- fast;
- deterministic;
- easy to maintain.

### Integration Tests

Purpose:

- validate interactions between components;
- verify collaboration between modules.

Examples of concerns:

- component communication;
- state transitions;
- workflow behavior.

### End-to-End Tests

Purpose:

- validate complete user-facing scenarios.

Examples:

- complete workflows;
- major system capabilities.

End-to-end tests are valuable but expensive. They should focus on a small number
of critical paths rather than reproducing every unit or integration scenario.
When a unit or integration test can provide the same confidence at lower cost,
prefer the lower level.

## 4. Testing Impact Assessment for Code Changes

Before completing a change, the Coding Agent must consider testing impact and
determine:

- whether new tests are required;
- whether existing tests need updates;
- whether regression tests are necessary.

Testing impact must be considered for changes involving:

- bug fixes;
- behavior changes;
- public API changes;
- core functionality;
- error handling or lifecycle behavior.

A change that affects behavior but introduces no test consideration is a signal
that the testing impact was not actually assessed, not that no testing is
needed.

## 5. Agent Behavior Testing

This section is critical for Synonz because agent behavior may contain
controlled variability. Agent systems should be tested based on behavior and
guarantees, not only exact outputs.

Agent behavior testing should consider:

- tool usage;
- state transitions;
- error handling;
- permission boundaries;
- task completion;
- recovery behavior.

Avoid requiring:

- identical natural language output;
- identical internal reasoning paths.

Requiring identical generated text or identical reasoning sequences produces
fragile tests that fail on acceptable variation. Tests should assert the
constraints and guarantees that must hold, not the exact surface that may
legitimately vary. When controlled variability is part of the design, tests must
verify the bounds of that variability, not a single fixed output.

## 6. Deterministic Testing for Non-Deterministic Systems

For systems with AI-driven or otherwise non-deterministic behavior, tests
should focus on:

- expected constraints;
- valid outcomes;
- required behaviors;
- system guarantees.

Avoid fragile tests based on:

- exact generated text;
- exact reasoning sequence;
- unnecessary timing assumptions.

When non-determinism is inherent, make it controllable in tests through fixed
seeds, deterministic fakes, or scoped assertions on properties rather than exact
output. If non-determinism cannot be controlled for testing, the test should
verify properties that hold across the allowed variation, not a single sample.

## 7. Async and Concurrency Testing

Because Synonz is built with Rust and asynchronous execution, tests must
consider:

- async workflows;
- concurrent execution;
- cancellation;
- timeout handling;
- race conditions;
- synchronization behavior.

Avoid:

- unnecessary sleeps;
- timing-dependent fragile tests.

Sleeps and timing assumptions create flaky tests. Prefer deterministic
synchronization primitives, controlled concurrency, and explicit ordering. When
cancellation or timeout behavior is part of the contract, test it explicitly
rather than relying on incidental timing. Lifecycle and cancellation behavior
of public async APIs must be tested, not merely documented.

## 8. Regression Testing

Bug fixes should preserve the lesson learned from failures. When appropriate:

1. reproduce the issue with a test;
2. implement the fix;
3. ensure the regression test prevents recurrence.

Regression tests should protect important behavior, not merely demonstrate that
a fix works once. A regression test that does not fail without the fix is not a
regression test. Regression tests must be deterministic and focused on the
behavior that was broken.

## 9. Test Quality Principles

Tests are also production-quality code. Tests should be:

- readable;
- maintainable;
- reliable;
- focused.

Avoid:

- duplicated test logic;
- fragile assumptions;
- excessive mocking;
- tests that only verify implementation details.

Excessive mocking couples tests to internal implementation and produces tests
that break on harmless refactors. Tests should verify behavior through stable
interfaces. When a test requires extensive mocking to run, that is a signal to
reconsider whether the unit under test has a clear, testable boundary.

## 10. Coding Agent Testing Workflow

The Coding Agent should follow:

1. Understand the requested change.
2. Identify testing impact.
3. Add or update appropriate tests.
4. Run validation.
5. Report test results.

The Coding Agent should not:

- skip testing considerations;
- remove tests without justification;
- modify tests only to make failures disappear;
- claim validation passed unless it actually ran.

When a test fails, the Coding Agent must distinguish between a test defect and a
code defect. Modifying a test to make a failure disappear without understanding
the cause is prohibited.

## 11. Review Agent Testing Checklist

The Review Agent should verify:

### Test Coverage Quality

- Are important behaviors tested?
- Are critical paths protected?

### Test Correctness

- Do tests verify intended behavior?
- Do assertions express real guarantees?

### Regression Protection

- Are previous failures protected?
- Do regression tests fail without the fix?

### Stability

- Are tests deterministic and maintainable?
- Are flaky tests identified and fixed, not tolerated?

### Architecture Alignment

- Do tests reflect approved architecture decisions?
- Do tests verify the guarantees the architecture intended to provide?

## 12. Relationship With Other Rules

### Architecture Rule

`.opencode/rules/architecture.md` defines what should be built and why. It
governs architectural decision discovery, trade-off evaluation, and ADR
generation.

### Coding Rule

`.opencode/rules/coding.md` defines how approved decisions should be
implemented. It governs code quality, Rust discipline, public API behavior, and
change scope control.

### Testing Rule

This rule defines how correctness and behavior should be verified. It governs
test levels, behavior verification, regression protection, and determinism
principles.

Responsibilities are deliberately separated. The Testing Rule does not define
code quality standards, the Coding Rule does not define test strategy, and the
Architecture Rule does not dictate implementation or testing details. Where a
topic spans multiple rules, the rule with the clearest mandate for that topic
takes precedence; conflicts are resolved by `AGENTS.md`.
