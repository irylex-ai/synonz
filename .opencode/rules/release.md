# Release Governance Rule

Operational rule for how Synonz prepares, validates, documents, and publishes
stable software releases. This is a specialized workflow under `AGENTS.md`;
where conflict appears, `AGENTS.md` takes precedence. This rule guides release
preparation, readiness evaluation, and publication governance.

This rule defines Release Governance. It is not a Release Agent configuration,
a CI/CD implementation guide, a Git tutorial, a packaging tutorial, or a specific
deployment workflow. Tooling and pipeline choices are made separately and are
not dictated here.

## 1. Purpose

A release is a product delivery event. A release is not only a code state; it
represents a stable software version, a documented change set, a validated
artifact, and a communication event with users.

A release should be traceable. Users and maintainers should be able to
understand:

- what changed;
- why it changed;
- what decisions influenced the change;
- how the release was validated.

The relationship is:

```
Version
  ↓
Changes
  ↓
Architecture Decisions (when applicable)
  ↓
Implementation
  ↓
Validation
```

Release quality is more important than release frequency. Avoid rushing
releases. Prefer stable, understandable, maintainable releases.

## 2. Release Principles

- Releases represent stable milestones.
- Releases require engineering confidence.
- Releases require clear communication.
- Releases should preserve project trust.

A release commits the project to a public contract. Once published, a version
becomes a dependency point that downstream users rely on. The cost of
correcting a published release is far higher than the cost of delaying it.

## 3. Release Lifecycle

The release lifecycle uses a simple three-state model:

```
PLANNING
  ↓
READY
  ↓
RELEASED
```

States are not multiplied beyond what is needed to govern publication. A
release moves forward through these states; backward transitions are allowed
only with explicit justification and recorded cause.

### PLANNING

A release target has been identified. PLANNING includes:

- target version;
- release scope;
- major goals;
- expected changes.

PLANNING is the state where scope is defined, changes are integrated, and the
release objective is stabilized. Scope may be refined during PLANNING, but a
release should not leave PLANNING until its objective is clear.

### READY

The release satisfies all requirements for publication. READY includes:

**Code readiness**

- implementation completed;
- required changes integrated;
- scope remains aligned with the original objective.

**Testing readiness**

- required tests completed;
- regression risks evaluated;
- important behaviors verified.

**Documentation readiness**

- required documentation updated;
- release notes prepared;
- migration guidance documented when needed.

**Distribution readiness**

- release artifacts prepared;
- version consistency verified.

READY is a condition, not a temporary approval step. A release is READY only
when all readiness criteria are satisfied; partial readiness is not READY.

### RELEASED

The version has been publicly published. RELEASED includes:

- release announcement;
- published artifacts;
- accessible version information.

Once RELEASED, a version is an immutable public artifact. Corrections to a
released version create a new version, not a rewrite of the existing one.

## 4. Version Management

Synonz uses Semantic Versioning. Version numbers follow the form
`MAJOR.MINOR.PATCH`.

### MAJOR

For incompatible changes. Examples:

- breaking public API changes;
- incompatible behavior changes.

### MINOR

For backward-compatible features. Examples:

- new capabilities;
- new extensions.

### PATCH

For backward-compatible fixes. Examples:

- bug fixes;
- security fixes.

A version number communicates the kind of change, not the amount of effort.
Breaking public API changes require a MAJOR bump regardless of how small the
diff is. Pre-1.0 compatibility expectations should be explicitly documented
when stability guarantees are needed; do not assume pre-1.0 stability until
stability expectations have been explicitly stated for the version in question.

## 5. Release Readiness Criteria

The conditions required before entering READY:

### Implementation

- intended changes completed;
- scope remains aligned with the original objective;
- no unintended scope expansion absorbed into the release.

### Validation

- tests completed;
- important behaviors verified;
- regressions considered;
- regression tests added for fixed bugs when applicable.

### Documentation

- user-facing documentation synchronized;
- release notes prepared;
- breaking changes documented.

### Compatibility

- breaking changes identified;
- version impact follows Semantic Versioning;
- migration requirements documented when necessary.

## 6. Release Note Requirements

Release Notes are user-facing communication, not a commit log. Release Notes
should explain:

### Summary

What this release provides.

### Highlights

Important new capabilities or improvements.

### Breaking Changes

Changes requiring user attention. Breaking changes must be explicitly
identified. If a release contains no known breaking changes, this should be
stated clearly when appropriate, so that the absence of listed breaking changes
is not mistaken for an accidental omission.

### Migration Guidance

Required migration actions, if any.

### Bug Fixes

Important corrections.

Release Notes should be written for users who will upgrade, not for
maintainers who already know the history. Commit-message-level detail belongs
in the changelog, not in the Release Notes summary.

## 7. Release Note Generation and Human Review

AI may assist with generating Release Note drafts. AI-generated release notes
must be reviewed and confirmed by maintainers before public release.

Human confirmation is an approval action. It is NOT a lifecycle state. The
lifecycle remains:

```
PLANNING → READY → RELEASED
```

Human confirmation is a required activity that must occur before a release
moves from READY to RELEASED. It does not add a state to the lifecycle, for
the same reason review is not a documentation lifecycle state: approval is an
activity that may happen multiple times and does not represent a stable
lifecycle stage.

## 8. Breaking Change Management

- Breaking changes must be explicitly identified.
- Version impact must follow Semantic Versioning.
- Migration guidance should be provided when required.

Breaking changes are never silent. A change that affects the public contract is
breaking even if the implementation diff is small. When a breaking change is
genuinely required, it must be:

- marked as breaking in the Release Notes;
- reflected in the version number per Semantic Versioning;
- accompanied by migration guidance where users must act.

## 9. Documentation Synchronization

Documentation Rule manages project documentation. Release Rule manages
version-specific release communication.

### Documentation Rule scope

- README
- guides
- architecture documents
- API documentation

### Release Rule scope

- changelog
- release notes
- migration notes

Project documentation must be synchronized with the state of the release
before READY. Release-specific communication is generated per release. The two
are complementary: documentation describes the stable product; release notes
describe what changed in this version. Detailed documentation lifecycle,
localization, and quality rules are defined in `.opencode/rules/documentation.md`
and take precedence over this section for those topics.

## 10. Artifact and Distribution Verification

Before RELEASED, published artifacts must be verified. Verification includes:

- artifact correctness;
- version consistency;
- availability for users.

Do not assume an artifact is correct because it was built. Version strings,
metadata, and published outputs must be checked against the intended release.
After release, additional verification may happen only when required by
distribution platforms or operational needs; routine post-release activity
belongs to Section 12. Tooling choices are not dictated here; use whatever the
repository actually provides.

## 11. Release Workflow

The general workflow:

1. Define release goal and scope (PLANNING).
2. Prepare changes (PLANNING).
3. Verify readiness against all release readiness criteria.
4. Generate release notes.
5. Human review and confirmation.
6. Publish release.

Steps 1–4 occur within PLANNING. Step 3 evaluates readiness; when all release
readiness criteria are satisfied, the release is READY. Step 5 is the required
human confirmation activity that must occur before a READY release moves to
RELEASED; it is not a separate lifecycle state. Step 6 moves the release to
RELEASED.

## 12. Post Release Activities

After release:

- communicate release information;
- monitor important issues;
- capture follow-up improvements.

A RELEASED release is not the end of governance. Issues discovered after
release are handled as new work in a subsequent release. Follow-up improvements
captured during release preparation should be recorded as future work, not
silently absorbed into the current release.

## 13. Relationship With Other Rules

### Architecture Rule

`.opencode/rules/architecture.md` defines why and how major technical
decisions are made.

### Coding Rule

`.opencode/rules/coding.md` defines how approved changes are implemented.

### Testing Rule

`.opencode/rules/testing.md` defines how correctness and stability are
verified.

### Documentation Rule

`.opencode/rules/documentation.md` defines how project knowledge is maintained.

### Release Rule

This rule defines how completed software changes are delivered to users.

Responsibilities are deliberately separated. The Release Rule does not define
architecture, code quality, test strategy, or project documentation. Each of
those belongs to its own rule. Where a topic spans multiple rules, the rule
with the clearest mandate for that topic takes precedence; conflicts are
resolved by `AGENTS.md`.
