# Coding Governance Rule

Operational rule for transforming approved Synonz architecture decisions into
maintainable, safe, and high-quality Rust code. This is a specialized workflow
under `AGENTS.md`; where conflict appears, `AGENTS.md` takes precedence. This
rule guides the Coding Agent, Review Agent, and human contributors.

This rule defines coding governance principles, not implementation design. It
does not specify modules, crates, APIs, or architecture decisions. Approved
architecture decisions are made through the process defined in
`.opencode/rules/architecture.md`.

## 1. Purpose

Code quality is not only about making software work. Code must also be
maintainable, understandable, safe, testable, evolvable, and suitable for
long-term open-source collaboration. Prefer clarity over cleverness, simplicity
over unnecessary abstraction, and maintainability over short-term convenience.

## 2. Code Quality Principles

- Readable code over clever code.
- Explicit design over hidden behavior.
- Small, focused changes.
- Avoid unnecessary complexity.
- Avoid premature optimization.

Code should serve long-term maintainability. A reader without private project
knowledge should be able to understand the intent and behavior of a change.
Cleverness that obscures intent is a defect, not an achievement. Performance
optimization should be introduced only when justified by measurement and a real
requirement, not by speculation.

## 3. Rust Development Principles

### Stable Rust Preference

- Prefer stable Rust.
- Avoid unnecessary nightly features.
- Nightly usage requires explicit justification, documented invariants, and a
  plan for returning to stable where practical.

Do not introduce nightly features merely for convenience.

### Ownership and Borrowing Design

Design ownership models intentionally. Avoid:

- unnecessary cloning;
- excessive `Arc` usage;
- excessive locking;
- fighting the borrow checker instead of improving the design.

Prefer:

- clear ownership;
- explicit lifetimes when they add correctness without harming clarity;
- simple, traceable data flow.

When the borrow checker rejects a design, the first response should be to
reconsider the design, not to suppress it with cloning or interior mutability.

### Error Handling

Library code and framework components should avoid uncontrolled use of
`unwrap()` and `expect()`, especially in public APIs and reusable paths.
Prefer:

- `Result`-based error handling;
- meaningful, appropriately typed error types;
- actionable error messages that preserve useful context.

`unwrap()` and `expect()` may be acceptable for genuinely unreachable internal
invariants, with a documented justification. They are not acceptable for
expected runtime failures or public API failure paths. See `AGENTS.md` for the
panic policy; this rule does not contradict it.

## 4. Public API Discipline

Because Synonz is a framework, public APIs are long-term contracts. Public API
changes should consider:

- backward compatibility;
- stability;
- documentation impact;
- ecosystem impact on downstream users.

Avoid unnecessary breaking changes. When a breaking change is genuinely
required, it must be explicit, justified, documented, and accompanied by a
migration path where practical. Public API surface should be minimal and
intentional; implementation details should remain private unless there is a
strong reason to expose them.

## 5. Module and Crate Design

- Clear responsibility boundaries.
- Avoid circular dependencies.
- Prefer dependency direction clarity.
- Avoid unnecessary coupling.

A module or crate should have a clear, single purpose. When a unit begins to
serve multiple unrelated concerns, that is a signal to reconsider its
boundaries rather than to accumulate more responsibilities. Circular
dependencies are a design defect, not a build problem to work around.

## 6. Async Programming Principles

Agent systems commonly require asynchronous execution. Observe the following
rules:

- Define async boundaries explicitly; do not spread `async` indiscriminately.
- Avoid blocking operations inside async contexts.
- Treat cancellation as an explicit lifecycle concern; document cancellation
  safety of public async APIs.
- Handle timeouts explicitly where waiting is involved.
- Make concurrency safety explicit; document `Send`/`Sync` implications where
  relevant.

Avoid introducing async complexity without necessity. A synchronous path that
is correct and clear is preferable to an async path added speculatively.

## 7. Dependency Management

Before adding a dependency, consider:

- maintenance activity;
- community adoption;
- license compatibility with Apache License 2.0;
- security impact and history;
- long-term sustainability;
- transitive dependency impact;
- build and compile impact.

Avoid unnecessary dependencies. Do not add a dependency to avoid implementing
a small, well-understood capability. Do not add multiple dependencies that
solve substantially overlapping problems. Dependency introduction that affects
architecture or public APIs should be validated through the architecture
process (see `.opencode/rules/architecture.md`).

## 8. Testing Expectations

- New functionality should include appropriate tests.
- Bug fixes should include regression tests when applicable.
- Public APIs should have meaningful coverage.
- Tests should verify behavior, not implementation details.

This section defines coding-level expectations only. The full testing strategy,
including test levels, proportionality, and determinism rules, belongs to the
Testing Rule and is not duplicated here.

## 9. Coding Agent Workflow

The Coding Agent should follow:

1. Understand the requested change.
2. Inspect existing code and architecture.
3. Identify relevant constraints.
4. Plan the implementation approach.
5. Make focused changes.
6. Run appropriate validation.
7. Explain the changes.

The Coding Agent should not:

- blindly modify files without understanding context;
- introduce unnecessary refactoring;
- ignore approved architecture decisions;
- silently introduce breaking changes;
- claim validation passed unless it actually ran.

When implementation and approved architecture diverge, the Coding Agent must
flag the conflict rather than silently resolving it. Significant implementation
decisions that were not anticipated by the architecture should be raised, not
absorbed.

## 10. Change Scope Control

Code changes should remain within the intended scope of the request. The
Coding Agent must optimize for correctness, focused changes, and minimal
unintended impact. Uncontrolled task expansion, unnecessary refactoring,
hallucination-driven modifications, and architecture drift introduced through
implementation changes are all defects.

### Coding Agent Responsibilities

The Coding Agent must:

- understand the requested objective before modifying code;
- inspect relevant existing code and constraints;
- identify the expected change boundary;
- minimize unrelated file and module changes;
- explain any necessary scope expansion.

### Prohibited Behaviors

The Coding Agent must not:

- expand tasks based on assumptions;
- introduce unrelated refactoring;
- modify architecture without following the Architecture Rule
  (`.opencode/rules/architecture.md`);
- change public APIs without considering compatibility impact;
- implement "future improvements" that are outside the requested objective.

### Handling Additional Findings

When the Coding Agent discovers possible improvements, design issues,
unrelated technical debt, or potential refactoring opportunities, it should:

1. report the finding;
2. explain the potential impact;
3. propose a separate follow-up task.

It should not silently include those changes in the current implementation.
Findings that touch architecture or public APIs must be routed through the
Architecture Rule, not absorbed into a coding change.

## 11. Refactoring Principles

- Prefer incremental changes.
- Avoid unrelated modifications in the same change.
- Preserve existing behavior unless change is intentional.
- Explain significant refactoring decisions.

Refactoring should be a separate, focused change when practical, not mixed
into feature work. Behavior-preserving refactors should not be bundled with
behavior changes in a way that obscures review. Significant refactors that affect
public APIs, ownership models, or async boundaries must be explained and
justified.

## 12. Code Review Checklist

### Correctness

- Does the code solve the intended problem?

### Maintainability

- Is the design understandable without private knowledge?
- Are responsibilities clear?

### API Impact

- Does this affect public contracts?
- Are breaking changes explicit and justified?

### Error Handling

- Are failures handled properly?
- Are error messages meaningful and actionable?

### Performance

- Are performance considerations reasonable?
- Is optimization justified rather than speculative?

### Security

- Are security risks considered?
- Are secrets, unsafe execution, and exposure avoided?

### Architecture Alignment

- Does the implementation follow approved architecture decisions?
- Are deviations explicitly flagged?

## 13. Relationship With Other Rules

### Architecture Rule

`.opencode/rules/architecture.md` defines what should be built and why. It
governs architectural decision discovery, trade-off evaluation, and ADR
generation.

### Coding Rule

This rule defines how approved decisions should be implemented. It governs
code quality, Rust discipline, public API behavior, and review criteria.

### Testing Rule

The Testing Rule (to be introduced) defines how correctness should be verified.
It governs test levels, coverage expectations, and determinism rules.

Responsibilities are deliberately separated. The Coding Rule does not define
architecture, the Architecture Rule does not dictate implementation details,
and the Testing Rule does not restate coding expectations. Where a topic spans
multiple rules, the rule with the clearest mandate for that topic takes
precedence; conflicts are resolved by `AGENTS.md`.
