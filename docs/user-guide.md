# User guide

## First useful result

Setup is complete only when Universal Context Manager can compose at least one durable entry for a
real project. A healthy daemon with an empty library is not a completed onboarding.

1. Select the repository where the context should apply.
2. Preview detected instruction files or create one entry manually.
3. Confirm the destination scope and review policy.
4. Apply or approve the entry.
5. Inspect Effective Context and verify the selected adapter can retrieve it.

## Library

The Library contains approved durable entries. Packs group related entries internally, but each
entry keeps its own content, provenance, lock state, revision history, and lifecycle status.

Use project scope for repository-specific architecture, commands, and conventions. Use task scope
for temporary handoff state. Use global scope only when guidance should apply to every connected
project.

## Inbox

The Inbox contains proposals waiting for a human decision. A proposal explains:

- why review was required
- who or what proposed it
- its destination scope and pack
- existing and proposed content
- provenance and request time

Approve or reject multiple compatible items together. Editing remains a single-item action so the
final content is explicit. Every accepted mutation creates a new revision.

## Effective Context

Effective Context is the exact read-only result delivered to an adapter. It is ordered:

1. global
2. project
3. task

The view includes entry identifiers, revisions, provenance, exclusions, warnings, estimated size,
and the rendered Markdown. Consumers should display this backend result directly rather than
reconstructing it.

## Search

Search covers entries, pending reviews, revisions, activity, and connections. Opening a result
navigates to the underlying item so it can be inspected or changed.

## Connections

Connection health is based on executable, daemon, MCP handshake, version, and plugin checks. A
harness configuration directory by itself is not proof that an adapter works.

Use `contextctl doctor` for the same diagnostics in a terminal. Safe repair actions never rewrite
third-party harness configuration without an explicit installation command.

## Review policy

- **Strict:** every non-duplicate proposal waits for review.
- **Balanced:** safe project/task proposals apply; global, conflicting, and locked proposals wait.
- **Fast:** project/task conflicts may apply; global and locked proposals still wait.

Secret rejection applies in every mode.

## Privacy and data

UCM storage and indexing remain local. Context selected for an adapter can still be transmitted by
that harness to its configured model provider. The database is protected by local account and
filesystem permissions rather than application-level encryption.

Exports may include project paths and durable context. Preview their scope before sharing them and
store them with the same care as repository documentation.
