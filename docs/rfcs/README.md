# Requests for Comments (RFCs)

This directory holds **Requests for Comments (RFCs)**: structured proposals for
significant architecture changes that need broader input *before* a decision is
made. RFCs complement the [Architecture Decision Records](../adr/README.md) in
`docs/adr/`, which record decisions *after* they are made.

- An **RFC** proposes and debates a change.
- An **ADR** records the outcome once the change is accepted and implemented.

An accepted RFC must result in a corresponding ADR once implemented, closing the
loop between proposal and recorded decision. This process applies to *future*
changes only; past architecture decisions are not retroactively turned into RFCs.

## When an RFC is required

Open an RFC instead of a direct PR when a change meets **any** of the following
triggers:

- Introduces or replaces a core subsystem or service (e.g. the swap engine,
cross-chain bridge interface).
- Changes a public interface, wire format, or on-chain contract that other
components or external consumers depend on.
- Adds a new external dependency, protocol, or network integration that is
architecturally load-bearing.
- Changes data models, storage, or migration strategy in a way that is hard to
reverse.
- Affects security, key management, or trust assumptions.
- Is expected to require coordinated work across multiple teams or repositories.

A change does **not** need an RFC when it is a bug fix, a localized refactor, a
dependency bump, documentation, or a change fully contained within one module
with no external interface impact. When in doubt, open an RFC — a short one is
cheap, and reviewers can redirect you to a direct PR if it is unnecessary.

## Submitting an RFC

1. Copy [`000-template.md`](./000-template.md) to a new file named
   `NNNN-short-title.md`, where `NNNN` is the next zero-padded number after the
   highest existing RFC (e.g. `0001-swap-engine-interface.md`).
2. Fill in every required section: **Problem**, **Alternatives considered**,
   **Proposed approach**, and **Open questions**. Keep it lightweight — a few
   paragraphs per section is enough to start the conversation.
3. Open a pull request against `docs/rfcs/` with the RFC file. The PR is the
   submission; the discussion happens on the PR and any linked issue.
4. Set the RFC status to `Draft` in the header.

## Review and comment period

The review period is **at least 14 calendar days** from the date the RFC PR is
opened, unless a maintainer explicitly shortens it for a low-risk change. During
this period:

- Reviewers leave comments on the PR; authors revise the RFC in place and note
  substantive changes in the PR description.
- The RFC status moves from `Draft` to `Review` once the author considers it
  ready for a decision.
- Consensus is reached when there are no unresolved blocking objections from
  maintainers. Silence is not consent: at least one maintainer must explicitly
  approve.

## RFC states

Every RFC has exactly one status in its header:

- **Draft** — being written; not yet ready for review.
- **Review** — open for the comment period and awaiting a decision.
- **Accepted** — consensus reached; implementation may begin. An ADR must be
  added under `docs/adr/` once the change is implemented, and the RFC should link
  to it.
- **Rejected** — consensus is that the change should not proceed. The RFC stays
  in the repository with the rationale recorded.
- **Withdrawn** — the author has abandoned the proposal.
- **Deferred** — no consensus and no active work; the proposal is parked. A
  deferred RFC must name a reason and a date, and may be revived by reopening a
  PR that moves it back to `Review`.

No RFC may remain indefinitely in `Review`. If an RFC has had no activity for
**30 days** and no consensus is in sight, a maintainer moves it to `Deferred` or
`Rejected` and records why. This keeps the open-RFC list meaningful.

## Closing the loop with ADRs

When an accepted RFC is implemented, add an ADR under `docs/adr/` following
[`docs/adr/000-template.md`](../adr/000-template.md). The ADR should reference
the RFC, and the RFC's status should be updated to link back to the ADR. If
implementation diverges from the accepted RFC, note the divergence in the ADR.

## Index

| RFC | Title | Status | ADR |
| --- | ----- | ------ | --- |
| [0001](./0001-rfc-process.md) | Formalize the RFC process | Accepted | — |
