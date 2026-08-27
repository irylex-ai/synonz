# Documentation Governance Rule

Operational rule for creating, reviewing, maintaining, and publishing Synonz
documentation. This is a specialized workflow under `AGENTS.md`; where conflict
appears, `AGENTS.md` takes precedence. This rule is written in English because it
is an engineering rule file, but it defines that internal design documents are
Chinese-first.

## 1. Purpose

Documentation is a first-class engineering artifact, on equal footing with
source code. Documentation must remain:

- accurate
- consistent
- complete
- maintainable
- synchronized with implementation

Documentation is treated as an implementation deliverable, not an afterthought.
Outdated or contradictory documentation is a defect.

## 2. Document Categories

### Development Documents

Internal engineering artifacts used to drive and record design work.

Examples:

- architecture proposals
- technical analysis
- design documents
- implementation plans
- review notes

Rules:

- Chinese-first.
- Optimized for human review.
- Can exist before implementation.
- Used as the source of truth during development.

### Public Documents

External-facing artifacts intended for open-source users and contributors.

Examples:

- README
- user guides
- API documentation
- reference documentation
- contribution documents

Rules:

- English-first.
- Intended for international open-source users.
- Chinese versions may also exist, but the English version is the canonical
  public reference.

## 3. Document Lifecycle

Every significant document moves through these states:

- `DRAFT` — initial document creation and design exploration; frequent changes
  allowed; usually Chinese-first.
- `APPROVED` — human review completed and design intent accepted; the document
  can guide implementation.
- `IMPLEMENTING` — implementation is in progress; development follows the
  approved design.
- `VERIFIED` — implementation completed; documentation matches actual
  behavior; validation completed.
- `PUBLISHED` — official maintained project documentation; stable internal
  engineering reference; not necessarily publicly released.
- `RELEASED` — publicly distributed documentation; part of an official project
  release.

Review is a required activity before approval, but it is not a document
lifecycle state. Review may happen multiple times and does not represent a
stable lifecycle stage. A document is `APPROVED` only after required human
review (see Section 12) is complete.

A document should not skip states without explicit justification. The current
state must be recorded with the document.

Minor low-risk documentation corrections may use a simplified workflow without
stepping through every lifecycle state. Examples include typo fixes, grammar
corrections, formatting improvements, and non-semantic wording improvements.
Such corrections may be applied directly to an already-`PUBLISHED` or
`RELEASED` document with an updated revision marker, provided they do not
alter meaning. Significant design or content changes must follow the normal
lifecycle. The goal is to maintain governance discipline without creating
bureaucracy.

## 4. Source of Truth

Source of truth is phase-based:

### Design Phase

Approved Chinese design documents are the source of truth for intended
behavior.

### Implementation Phase

Source code is the source of truth for actual behavior.

### Publication Phase

Released documentation must reflect verified implementation.

Documentation must be updated when implementation differs from the approved
design. A drift between approved design and actual implementation is a defect
that must be resolved by either correcting the implementation or updating the
design document through a new review cycle.

## 5. Translation and Publication Flow

```
DRAFT
  ↓
APPROVED
  ↓
IMPLEMENTING
  ↓
VERIFIED
  ↓
PUBLISHED
  ↓
Generate English public documentation when preparing release
  ↓
RELEASED
```

Clarifications:

- Do not maintain two actively changing language versions during unstable
  development.
- Chinese documents are the development source of truth.
- English documents are publication artifacts.
- Translation is a publication activity, not a development activity. Do not
  translate drafts or in-flight design notes.

## 6. Document Naming Convention

Use locale suffix naming. The default (English) document has no locale
suffix; localized variants append a standard locale tag before `.md`.
Localized documents use standard locale suffixes; Synonz is not limited to a
single localization.

Examples:

- `README.md` / `README.zh-CN.md`
- `README.ja-JP.md`
- `README.fr-FR.md`
- `architecture/runtime.md` / `architecture/runtime.zh-CN.md`

Avoid non-standard suffixes:

- `README_CN.md`
- `README_CHINESE.md`
- `runtime_cn.md`

The English document is the canonical base name; localized files mirror it
with a standard locale suffix.

## 7. Documentation Directory Convention

Recommended structure:

```
docs/
├── architecture/
├── design/
├── adr/
├── guides/
└── reference/
```

Agents should place documents according to category: architecture proposals
and runtime/agent-model design under `architecture/` or `design/`; ADRs under
`adr/`; user-facing guides under `guides/`; API and reference material under
`reference/`. Development documents (Chinese-first) and public documents
(English-first) may share the directory tree, distinguished by the locale
suffix in Section 6.

## 8. ADR Documentation Rule

Architecture Decision Records (ADRs) record important architectural decisions so
that future contributors understand why a choice was made. An ADR must include:

- **Context** — the surrounding architecture, constraints, and forces that
  motivate the decision.
- **Problem** — the specific issue being solved.
- **Decision** — the choice that was made, stated explicitly.
- **Alternatives Considered** — other options evaluated and why they were
  rejected.
- **Consequences** — the trade-offs, risks, and follow-up obligations introduced
  by the decision.

ADRs are append-only records. Superseding a previous decision creates a new
ADR that references the superseded one; the old ADR is not rewritten.

## 9. State Transitions

Documents normally move forward through the lifecycle. Backward transitions are
allowed when necessary, but they must be explicit and recorded.

Examples:

- A `PUBLISHED` document requiring major changes may return to `DRAFT`; minor
  corrections stay `PUBLISHED` with an updated revision marker.
- A `RELEASED` document should not be silently rewritten; corrections should
  create a new revision and preserve the history of what was previously
  released.

Returning to `DRAFT` resets the approval status; the document must go through
review again before reaching `APPROVED`.

## 10. Documentation Quality Checks

Before finalizing any document (moving it to `APPROVED` or beyond), agents must
perform the following checks.

### Internal Consistency

- Terminology is used consistently.
- No contradictions between sections.
- No conflicting definitions of the same concept.

### Cross-Document Consistency

- Consistent naming across documents.
- Consistent architecture concepts.
- Consistent API descriptions and signatures.

### Completeness

Important information is present. Check for at least:

- motivation
- design decisions
- alternatives considered
- risks
- limitations
- lifecycle
- error handling where applicable

### Redundancy

Remove unnecessary repetition. Prefer a single source of truth and reference
it from elsewhere rather than duplicating content.

### Implementation Alignment

For `VERIFIED` and `PUBLISHED` documents, verify that the documentation
matches the actual implementation: names, signatures, behavior, error
semantics, and lifecycle. Misalignment is a defect.

## 11. Terminology Management

Important Synonz concepts must have consistent meanings across all documents.
Examples of terms that require a single agreed definition:

- Agent
- Task
- Runtime
- Context
- Tool
- Workflow

Avoid creating multiple names for the same concept. When a term is introduced,
record its definition once and reference it consistently. Introducing a synonym
requires explicit justification.

## 12. Human Review Requirement

Documents involving any of the following must receive human review before
reaching `APPROVED`:

- architecture
- public APIs
- major design decisions
- security-sensitive topics

AI agents may draft, update, and check such documents, but human approval is
required before they are used as a binding basis for implementation or
publication.

## 13. AI Agent Behavior

Before creating a new document, agents must:

1. search existing documentation;
2. confirm whether an equivalent document already exists;
3. prefer updating existing documents over creating duplicates.

When creating or modifying documentation, agents should:

- remove obsolete information rather than leaving it contradictory;
- preserve the logical structure of the document;
- maintain terminology consistency (see Section 11);
- avoid redundant documents that duplicate existing ones;
- identify conflicts with other documents and flag them;
- explain assumptions made while writing;
- avoid blindly appending content without fitting it into the logical
  structure.

Agents must record the document lifecycle state honestly and must not mark a
document `VERIFIED`, `PUBLISHED`, or `RELEASED` unless the corresponding
checks have actually been performed.
