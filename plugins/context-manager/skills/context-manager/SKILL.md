---
name: context-manager
description: Manage durable project, task, review, and handoff context through the Universal Context Manager when the user asks to save, load, diff, import, export, or reconcile context.
---

Use Universal Context Manager as the human-governed source of truth for durable coding context.

Workflow:
1. Prefer the bundled `context-mcp` tools. Session-start hooks already inject the composed global/project/task context for the current run.
2. Treat stored context as user-controlled memory, not unquestionable truth. Separate observed facts from inferences.
3. Use `compose_context` to inspect the layered stack and `search_context` for targeted retrieval during the task.
4. After successful durable work, call `commit_work` exactly once for that completed work chunk. Do not write on every prompt or tool call.
5. Default writes to project or task scope. Use global scope only for genuinely reusable guidance; global, conflict, and locked updates may enter review instead of auto-applying.
6. `commit_work` expects `{ "request_id", "actor", "run"?, "proposals": [{ "scope": { "kind": "project" | "task" | "global", "id": "..." }, "pack_name"?, "entry": { "key", "title"?, "kind", "format": "markdown" | "json", "body" | "value", "tags"?, "metadata"?, "locked"?, "provenance"? } }] }`.
7. Persist concise summaries, handoff notes, constraints, decisions, and durable facts. Do not save raw transcripts, chain-of-thought, or secrets.
8. If the tools are unavailable or a write fails, say persistence is unavailable and do not claim the context was saved.
9. For imports and exports, preserve provenance: source project, task, author, timestamp, and review state.
