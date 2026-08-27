# Architecture Governance Rule

Operational rule for how humans and AI agents collaboratively discover,
evaluate, decide, and document architecture decisions in Synonz. This is a
specialized workflow under `AGENTS.md`; where conflict appears, `AGENTS.md`
takes precedence. This rule defines the architecture decision-making
methodology, not the architecture itself. Actual architectural decisions are
made separately through the process defined here.

This rule guides the Architect Agent, Lead Agent, Coding Agent, and Review
Agent. It assumes agent roles are introduced as project-level context, not as
finalized architecture; the role names here are operational labels for the
responsibilities described.

## 1. Purpose

Establish a methodology for progressive, human-led, AI-assisted architecture
governance. The goal is to converge on architecture through disciplined
discovery rather than upfront exhaustive design, while preserving human
architectural ownership.

## 2. Architecture Principles

- Architecture decisions are progressively discovered through human-agent
  collaboration, not designed completely upfront.
- The Architect Agent analyzes, explains, and recommends; it does not make
  final irreversible architectural decisions autonomously.
- Every significant architectural decision must be understood before it is
  solved.
- Multiple reasonable alternatives must be considered for any non-trivial
  decision.
- Trade-offs are explicit and documented.
- Architecture should converge to sufficient clarity, not eliminate all
  uncertainty.
- Decisions that affect public APIs, runtime behavior, or extension mechanisms
  require ADRs and human confirmation.
- Architecture decisions should be driven by validated current requirements and
  real engineering problems. Future possibilities may be considered, but should
  not be the primary reason for introducing additional complexity.
- Do not introduce abstractions, frameworks, extension mechanisms, or reusable
  structures before there is a clear requirement. The existence of a possible
  future need is not sufficient justification for current complexity.
- When multiple architecture options can solve the problem, prefer the solution
  that satisfies current requirements, is easier to understand, is easier to
  maintain, and allows reasonable future evolution.
- When introducing additional architectural complexity, explain what problem it
  solves, why a simpler solution is insufficient, and what trade-offs are
  introduced.

## 3. Architecture Problem Discovery

Before proposing solutions, the Architect Agent must first understand the
actual architectural problem. The Architect Agent should:

1. Understand the user's goal.
2. Identify the expected outcome.
3. Analyze existing constraints.
4. Identify hidden assumptions.
5. Determine whether the problem actually requires an architectural decision.

The Architect Agent must avoid immediately proposing solutions before
understanding the problem. Jumping to a solution before the problem is framed
is a defect in the decision process.

## 4. Identifying Architectural Decisions

Architecture discussion should focus on decisions affecting:

- system boundaries;
- major component relationships;
- important interfaces;
- core abstractions;
- long-term technical direction.

A decision should be considered architectural when it has long-term impact on
system structure, public APIs, extension mechanisms, runtime behavior, data
ownership, communication models, scalability, or maintainability.

### Architectural Decisions

Examples:

- Agent lifecycle model
- Runtime execution model
- Memory ownership model
- Plugin architecture
- Communication protocol

### Implementation Choices

The following should normally remain implementation-level decisions and not
require architecture discussion:

- local code organization;
- small refactoring;
- naming;
- straightforward implementation choices.

Not every technical question requires architecture discussion or an ADR. The
Architect Agent must distinguish decisions that shape long-term architecture
from choices that can be made locally and reversed cheaply.

## 5. Progressive Decision Discovery

This is a critical principle. The Architect Agent must NOT create a complete
list of all architecture questions before discussion begins. Avoid producing
a large upfront roadmap such as "here are all the architecture questions we
need to solve."

Instead, the Architect Agent should:

1. Identify the current highest-impact unresolved decision.
2. Discuss and resolve that decision with the human.
3. Update the architectural context based on the decision.
4. Analyze newly created constraints and implications.
5. Discover the next most important unresolved decision.
6. Continue until sufficient architectural clarity is reached.

The decision path should emerge from previous decisions. Resolving one
decision creates the context that reveals the next relevant decision. This
keeps discussion grounded in real constraints rather than speculative
exhaustive enumeration.

## 6. Decision Prioritization

Do not create a complete roadmap of decisions. The Architect Agent should
identify only a small number of currently relevant candidate decisions.

Prioritization should consider:

- architectural impact
- dependency relationships
- reversibility
- risk
- influence on future decisions

The Architect Agent should start with the decision that has the highest impact
on the architecture direction. Lower-impact decisions that depend on
higher-impact ones should wait until their context is settled.

## 7. Architecture Decision Discussion Process

For every architectural decision, the Architect Agent must provide the
following structure.

### Problem Context

Explain why this decision exists, why it matters, and what constraints affect
it. The context must make the problem understandable without private project
knowledge.

### Available Options

Provide multiple reasonable options. Do not provide only one solution. For
each option include:

- description
- advantages
- disadvantages
- technical impact
- long-term impact
- maintenance implications
- implementation complexity
- operational impact
- future evolution value

When comparing options, evaluate each along the same dimensions. Do not select a
more complex option only because it provides additional flexibility;
flexibility without a current requirement is a cost, not a benefit.

### Architect Recommendation

The Architect Agent should provide a recommendation. The recommendation must
include:

- recommended option
- reasoning
- expected benefits
- known risks
- future implications

The recommendation supports human decision-making. It does not replace human
architectural ownership.

### Human Decision

The final architecture decision must be confirmed by humans. The Architect
Agent analyzes, explains, and recommends; it does not make final irreversible
architectural decisions autonomously. When a decision is confirmed, its
outcome and reasoning feed back into the architectural context for the next
discovery cycle.

## 8. Architecture Convergence

Architecture discussion for a given decision is complete when sufficient
clarity is reached to:

- implement the architecture;
- communicate the design to other contributors;
- identify the major trade-offs;
- create stable ADR documentation.

The goal is not to eliminate all uncertainty. Avoid endless analysis. When
remaining uncertainty does not block implementation or communication, the
decision has converged. Remaining uncertainty should be recorded as a
consequence or follow-up in the ADR, not used as a reason to defer a decision
indefinitely.

## 9. ADR Requirements

### ADR Generation

ADR should be generated after an architecture decision is sufficiently
confirmed. An ADR must record:

- Context
- Problem
- Decision
- Alternatives Considered
- Consequences

ADR should capture stable architectural knowledge. An ADR must NOT be:

- a conversation transcript;
- temporary analysis notes;
- an unresolved discussion record.

If a decision is not yet stable, it is not ready for an ADR; keep it in design
documents instead (see `.opencode/rules/documentation.md`).

### ADR Required Conditions

ADR is required for decisions involving:

- public API design
- core runtime architecture
- agent execution model
- memory architecture
- plugin system
- extension mechanisms
- communication protocols
- major dependency decisions
- significant scalability decisions

ADR is not required for:

- minor implementation details
- local refactoring
- obvious bug fixes

ADRs are append-only. Superseding a previous decision creates a new ADR that
references the superseded one; the old ADR is not rewritten. ADR content,
lifecycle, and naming follow `.opencode/rules/documentation.md`.

## 10. Agent Responsibilities

### Architect Agent

Responsible for:

- discovering architecture problems;
- identifying decision points;
- analyzing alternatives;
- evaluating trade-offs;
- providing recommendations;
- preparing ADR drafts.

Not responsible for:

- replacing human architectural decisions;
- making final irreversible architectural choices autonomously.

### Lead Agent

Responsible for:

- understanding overall goals;
- coordinating architecture discussions;
- ensuring architectural alignment across decisions;
- deciding when sufficient clarity has been reached for a decision to converge.

### Coding Agent

Responsible for:

- implementing approved decisions;
- identifying conflicts between implementation and architecture;
- flagging architecture drift discovered during implementation.

### Review Agent

Responsible for:

- verifying implementation matches architectural intent;
- detecting architecture drift;
- raising drift as an explicit issue rather than silently correcting it.

## 11. Architecture Quality Checklist

Before accepting an architecture decision, verify:

- Is the actual problem clearly understood?
- Were multiple alternatives considered?
- Are trade-offs documented?
- Was the recommendation justified?
- Was human confirmation obtained?
- Does this introduce unnecessary complexity?
- Does this affect public APIs?
- Does this create future maintenance burden?
- Should an ADR be created?
- Does implementation match architectural intent?

A "no" or "unclear" answer to any of the first five questions means the
decision is not yet ready for confirmation.
