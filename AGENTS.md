# AGENTS.md

Engineering constitution for the Synonz project. This document defines the
operational rules that humans and coding agents must follow when working on
Synonz. It is an engineering constitution, not an architecture specification.

## 1. Project Identity

Synonz is an open-source Agent Engineering Framework developed by irylex.

Synonz provides engineering primitives and infrastructure for building,
running, and orchestrating AI agents and multi-agent systems. The project is
open-source from day one, professional, long-lived, Rust-oriented, and designed
for external developers and downstream users.

Potential areas of development (not finalized architecture):

- agent abstractions, model integration, tool execution, context management
- task execution, lifecycle management, runtime, events
- multi-agent orchestration, agent communication, artifacts
- SDKs, CLI tooling

This list is not a module or crate structure.

## 2. Open-Source-First Principle

Develop the repository as if external developers will read the code, use the
framework, submit changes, depend on public APIs, and maintain it for years
without private knowledge. There is no "internal prototype" standard. Code
quality, API discipline, documentation, testing, security, maintainability,
dependency hygiene, and contributor experience are first-class requirements
from the first commit.

## 3. License

Synonz uses the **Apache License 2.0**.

- Project source code is distributed under Apache License 2.0.
- Dependency licenses must be compatible with Apache License 2.0.
- Do not copy third-party source code without verifying its license and
  attribution requirements.
- Preserve required copyright, license, and attribution notices.
- Do not remove or modify required third-party license notices.
- Generated or vendored third-party code requires licensing review.
- Do not assume public code is license-compatible.
- Flag unclear licensing before introducing the dependency or source.

The final `LICENSE` file will be added separately. Do not change the license
without an explicit project-level decision.

## 4. Engineering Philosophy

- Correctness over cleverness; simplicity before abstraction.
- Explicit behavior, lifecycle, and failure modes over implicit or magic.
- Small, composable abstractions with strong module boundaries.
- Minimal public surface area and predictable behavior.
- Testability, observability, and maintainability.
- Clear ownership and resource responsibility.
- Extensibility without speculative abstraction.

Avoid premature generalization, speculative architecture, unnecessary
abstraction layers or dependencies, hidden global state, implicit mutable
state, framework magic, tightly coupled modules, and convenience-driven API
pollution. Introduce abstractions only for real use cases, strong
architectural requirements, or demonstrated repeated patterns.

## 5. Framework-Specific Engineering Discipline

Synonz is a framework, not an end-user application. Public API quality is
critical. Behavior must be predictable, lifecycle semantics explicit, error
behavior understandable, and extension points clearly scoped. Implementation
details should remain private unless there is a strong reason to expose
them. Downstream developers must not need private project knowledge to use
public functionality. Design framework APIs for long-term evolution.

## 6. Public API Discipline

Treat public APIs as long-term contracts.

- Minimize `pub` surface; make visibility intentional.
- Distinguish public API from implementation details; do not leak internal
  types or assumptions.
- Prefer discoverable, composable APIs.
- Document public behavior, error semantics, and where relevant lifecycle
  and cancellation behavior.
- Consider compatibility and migration impact before changing public
  interfaces.

For significant public API changes, consider usability, discoverability,
composability, testability, extensibility, compatibility, migration impact,
and documentation impact. Do not define a semantic-versioning policy until
one is formally established.

## 7. Rust Engineering Standards

Synonz is a professional, long-lived Rust project.

- Prefer stable Rust unless a deliberate project-level decision establishes
  otherwise.
- Follow Cargo conventions; use clear workspace and crate boundaries when
  introduced.
- Respect strong module boundaries, visibility, and consistent naming.
- Prefer clear ownership and borrowing over unnecessary cloning.
- Design async and concurrency explicitly; document `Send`/`Sync`
  implications and treat cancellation as an explicit lifecycle concern.
- Design traits, generics, and lifetimes deliberately; avoid needless
  abstraction.
- Use feature flags only when genuinely useful.
- Manage dependencies conservatively (see Section 9).
- Format with `rustfmt`, lint with `Clippy`, and document public APIs with
  rustdoc.
- New warnings introduced by Synonz changes should be treated as defects and
  resolved whenever practical. Unrelated warnings originating solely from
  external dependencies do not require fixing.

Prefer safe Rust. Unsafe code is exceptional and requires a strong technical
justification, documented safety invariants, focused review, and appropriate
tests. Do not invent a toolchain version, MSRV, or toolchain policy until
explicitly established.

## 8. Error Handling and Panics

Errors should be explicit, meaningful, actionable, composable, appropriately
typed, and free from unnecessary information loss. Consider recoverable
versus unrecoverable errors, context preservation, propagation, public API
error semantics, logging versus returned errors, and cancellation versus
failure.

- Expected or recoverable runtime failures must be represented through
  appropriate error handling, not panics.
- Panics are not normal control flow.
- Panics may be appropriate for violated internal invariants or unrecoverable
  programmer errors.
- Public APIs should not rely on panics for expected failure behavior.
- Error messages should preserve useful context.

Do not prescribe a specific error crate until one is explicitly selected.

## 9. Dependency Management

Dependencies are part of the long-term architecture. When introducing a
dependency, evaluate necessity, maturity, maintenance activity, API stability,
license compatibility, transitive dependency impact, security history,
ecosystem adoption, platform compatibility, build and compile impact, and
long-term maintenance cost.

Avoid adding a dependency merely to avoid implementing a small, well-understood
capability. Avoid multiple dependencies that solve substantially overlapping
problems. Prefer stable, well-understood dependencies when the project needs
them. Do not prescribe specific libraries or a fixed dependency policy until
explicitly decided.

## 10. Architecture Principles

Architecture should be modular, composable, explicit, testable, observable,
and evolvable. Cover separation of concerns, dependency direction, ownership,
lifecycle, concurrency boundaries, state management, failure modes, error
propagation, cancellation, observability, and extension points.

Significant architectural decisions must be explicit. For significant
architectural changes: understand the problem, inspect relevant constraints,
identify affected boundaries, consider alternatives, explain trade-offs,
choose a design, implement incrementally, and verify behavior. Do not define
specific module names, crate names, protocols, runtimes, or communication
models here.

## 11. Architecture Decision Records

Significant architectural decisions should be documented using ADRs or an
equivalent mechanism. Potential topics include the agent execution model,
runtime model, lifecycle model, concurrency model, task model, communication
model, API model, extensibility model, storage model, observability model,
compatibility policy, dependency strategy, and security model. This section
establishes the process only; do not create ADR files or define specific
architecture decisions now.

## 12. Testing Strategy

Testing is a first-class requirement. Use the lowest-cost test level that
provides sufficient confidence; testing should be proportional to the risk
and scope of the change. Available levels include unit tests, integration
tests, API behavior tests, lifecycle tests, async/concurrency tests,
cancellation tests, error-path tests, regression tests, property-based tests
when appropriate, and end-to-end tests when appropriate.

Behavioral changes must include or update appropriate tests. Important
framework behavior must not rely only on manual testing. Tests should verify
externally meaningful behavior rather than implementation details when
practical. Tests must be deterministic whenever practical. Newly introduced
flaky tests are defects.

## 13. Documentation Standards

Documentation is part of implementation quality for public framework
behavior. Public Rust APIs must have appropriate rustdoc once they exist,
covering purpose, behavior, constraints, lifecycle, error behavior,
cancellation where applicable, and useful examples. Expect a README, public
API documentation, architecture documentation, ADRs, examples, development
documentation, configuration documentation, and migration documentation when
needed. Prefer a single source of truth; avoid duplicating information across
documents. Do not create documentation files now.

## 14. Examples and Developer Experience

Synonz should be understandable to developers who discover it through GitHub
or crates.io. Major framework capabilities should have runnable examples
when practical, prioritizing clarity, minimal setup, realistic usage,
correctness, copyability, and stable public APIs. Examples must use public
APIs, not private implementation details. Do not create example files now.

## 15. Security

Security is a first-class concern. Avoid hard-coded secrets, credentials in
source, sensitive data in logs, unsafe command execution, unnecessary
privileges, insecure defaults, accidental filesystem or network exposure,
bypassing security checks for convenience, and weakening authentication or
authorization without explicit justification. Security-sensitive behavior
must be explicitly considered whenever relevant. Do not invent a complete
security architecture now.

## 16. Observability

Important execution behavior must be designed with observability in mind.
When implementing execution-related functionality, consider lifecycle
visibility, structured events, tracing, task boundaries, timing, failures,
retries, cancellation, and resource usage. Do not select a specific
observability stack now.

## 17. Open-Source Contributor Experience

The repository must be understandable to external contributors without private
organizational knowledge. Prefer clear naming, predictable structure,
explicit behavior, minimal hidden state, useful documentation, tests near
relevant behavior, focused changes, and reviewable diffs. Do not rely on
undocumented tribal knowledge. Avoid comments that only explain private
organizational history.

## 18. Git and Change Management

Professional open-source Git practices are required: focused changes,
logically scoped commits, meaningful commit messages, reviewable diffs, no
unrelated modifications, no generated artifacts unless intentionally tracked,
no secrets, explicit handling of breaking changes, and documentation updates
when needed. Agents must not commit or push unless explicitly instructed by
the user.

## 19. Coding-Agent Behavior

Coding agents working on Synonz must:

- inspect before modifying and understand relevant code before meaningful
  changes;
- distinguish facts from assumptions;
- avoid inventing requirements, architecture, or premature design decisions;
- avoid unnecessary refactoring;
- keep changes focused and preserve established behavior unless change is
  intentional;
- explain important architectural decisions;
- add or update appropriate tests;
- verify actual results and never claim a test passed unless it actually ran;
- identify unresolved risks honestly;
- avoid silently introducing breaking changes;
- treat public API changes with additional care;
- prefer incremental implementation for large changes.

For substantial work, prefer **understand → design → implement → verify →
review**. Small, obvious changes may proceed directly when no meaningful
design uncertainty exists.

## 20. Language Policy

### Public Repository Artifacts

Public-facing repository artifacts should use English by default, including
source code, code comments, public API documentation, rustdoc, README,
CONTRIBUTING.md, user-facing guides, release documentation, AGENTS.md, agent
configuration and prompts, commit messages, and issue/PR templates. Use clear,
professional, international technical English. Do not unnecessarily translate
identifiers, filenames, commands, API names, crate names, or technical
terminology. The purpose is international open-source collaboration and external
developer accessibility.

### Internal Development Artifacts

Internal development artifacts may use Chinese-first when allowed by applicable
rules, including design documents, technical analysis documents, architecture
discussions, and development planning documents. The purpose is to improve
engineering communication and review efficiency during development. Detailed
rules for documentation lifecycle, localization, and naming conventions are
defined in `.opencode/rules/documentation.md` and take precedence over this
section for those topics.

### Human-Facing Communication

When communicating directly with the human developer, use Chinese by default.
Explain implementation results, decisions, trade-offs, risks, errors, and next
steps in Chinese. Preserve technical identifiers, filenames, commands, API
names, and necessary English terminology exactly. If the human explicitly
requests English, respond in English.

### Agent-to-Agent Communication

Internal agent instructions, prompts, structured task descriptions, technical
reports, and agent-to-agent coordination may use English when appropriate. All
internal agent communication is not required to be Chinese.

The key distinction: public artifacts are English-first; internal development
artifacts may follow applicable documentation rules; agent-to-agent
communication may use English when useful; human-facing responses use Chinese by
default. This policy does not require source code or public project
documentation to be written in Chinese.

## 21. Quality Gates

For meaningful changes, require appropriate verification such as
compilation, formatting, linting, unit tests, integration tests, regression
checks, documentation checks, public API review, and a final diff review. Use
only commands and tooling that actually exist in the repository. Do not invent
project-specific commands. Once the project's exact commands are established,
document them here.

## 22. Architectural Scope Control

This document is an engineering constitution, not an architecture
specification. It must not define final crate names, module names, agent
model, runtime implementation, task model, tool system, model-provider
abstraction, context architecture, communication protocol, storage
architecture, event architecture, concurrency implementation, dependency
stack, or observability stack. Coding agents must not invent such decisions
without sufficient project-level context. These decisions must be made later
through explicit architecture work.

## 23. Rule Files and Specialized Workflows

`AGENTS.md` is the highest-level engineering constitution. It defines stable
project-wide principles, coding philosophy, quality expectations, security
expectations, and agent behavior. It should NOT contain detailed workflow
procedures.

Specialized workflows live in dedicated rule files under `.opencode/rules/`.
These files define task-specific procedures and detailed operational rules,
such as documentation, architecture, release, security, and testing
workflows. Agents should load the rule files relevant to the current task
based on task context. Rule files must not contradict `AGENTS.md`; where
conflict appears, `AGENTS.md` takes precedence.

## 24. Project Engineering Rules

Detailed engineering rules are maintained separately under `.opencode/rules/`
and must be followed during relevant development activities. The following
rule files are the authoritative references for their respective domains:

- `.opencode/rules/architecture.md` — architecture decision discovery,
  trade-off evaluation, and ADR generation.
- `.opencode/rules/coding.md` — implementation principles, Rust development
  standards, code quality, Coding Agent behavior, and change scope control.
- `.opencode/rules/documentation.md` — documentation lifecycle, localization
  strategy, documentation quality, and ADR documentation requirements.
- `.opencode/rules/release.md` — release lifecycle, version management,
  readiness criteria, and release note governance.

This list is an entry point, not a substitute for the rules themselves. When a
task touches one of these domains, the corresponding rule file must be loaded
and followed. Rule files may be added or updated as the project matures; this
section should be kept in sync with the rules that exist.

## 25. Future Project Governance

As Synonz matures, the repository may add formal open-source governance and
tooling such as LICENSE, CONTRIBUTING.md, CODE_OF_CONDUCT.md, SECURITY.md,
CHANGELOG.md, issue templates, pull request templates, CI quality gates,
release process, and dependency and security automation. Do not create any of
these files now. The project's license is Apache License 2.0; the `LICENSE`
file will be added separately.
