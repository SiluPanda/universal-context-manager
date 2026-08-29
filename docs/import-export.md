# Import and export

Portable UCM bundles use `contextctl import`, `contextctl export`, and the desktop archive flow.
Existing harness instruction files use the staged `contextctl source-import` flow.

## Import existing instructions

Supported source families:

- `AGENTS.md`
- `CLAUDE.md` and `CLAUDE.local.md`
- `.github/copilot-instructions.md`
- `.github/instructions/*.instructions.md`
- `.cursor/rules/*.mdc` and `.cursorrules`
- `.continue/rules/*.md`
- ordinary Markdown when explicitly selected

Preview before applying:

```bash
contextctl source-import preview AGENTS.md --scope project
contextctl source-import apply AGENTS.md --scope project
```

Preview reports the detected source type, destination, generated entry keys, duplicates,
conflicts, review-policy outcome, and warnings. Apply is deterministic and skips unchanged
duplicates.

Markdown passed to the older `contextctl import --format markdown` command must be a UCM-exported
Markdown bundle containing UCM metadata markers. Ordinary Markdown is rejected there rather than
silently importing zero entries.

## Goals

- preserve provenance across repositories and harnesses
- keep human review in the loop
- separate observed facts, inferred summaries, and operator decisions

## Core bundle shape

```json
{
  "exported_at": "2026-08-17T00:00:00Z",
  "packs": [],
  "entries": [],
  "reviews": [],
  "runs": []
}
```

## Desktop archive flow

- the desktop app writes the native core JSON bundle selected by the operator
- native UCM JSON and UCM-exported Markdown are accepted as archive imports; instruction files use
  the separate staged preview flow
- entry provenance and review state remain explicit in exported records
- approved and rejected reviews retain their resolution note, timestamps, and current revision number on JSON round-trip
- imported packs restore their current description, metadata, lifecycle status, and governance lock state even when the destination already contains the pack
- imported runs retain their id, scope association, source, metadata, and start timestamp

## Scope isolation

Project and task filters apply to packs, entries, reviews, and runs. A project/task export also
includes the global layer needed to interpret its composed context, but never records from a
different project or task. A pack-name filter applies to packs, entries, and reviews; runs are
omitted because a run is not associated with an individual pack.

## Current fidelity boundary

The v1 bundle contains current packs, current entries, review records, and runs. It does not yet
carry complete historical revision chains. Imported pack and entry database identifiers, plus
entry database-managed creation/update timestamps, may be regenerated; pack and run chronology is
preserved. Export the original SQLite database when an exact forensic copy, rather than a portable
context bundle, is required.
