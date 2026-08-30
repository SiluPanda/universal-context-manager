#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
cd "$root"

for command in cargo git jq; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "required command not found: $command" >&2
    exit 1
  fi
done

cargo build --quiet --bins

workdir="$(mktemp -d "${TMPDIR:-/tmp}/ucm-e2e.XXXXXX")"
home="$workdir/home"
project="$workdir/demo-project"
socket="$workdir/d.sock"
mkdir -p "$home" "$project"
git -C "$project" init --quiet
daemon_pid=""
export CONTEXT_SOCKET_PATH="$socket"

cleanup() {
  if [ -n "$daemon_pid" ]; then
    kill "$daemon_pid" >/dev/null 2>&1 || true
    wait "$daemon_pid" >/dev/null 2>&1 || true
  fi
  rm -rf "$workdir"
}
trap cleanup EXIT HUP INT TERM

CONTEXT_MANAGER_HOME="$home" "$root/target/debug/contextd" --quiet &
daemon_pid=$!

attempt=0
while [ ! -S "$socket" ]; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 100 ]; then
    echo "contextd did not create its socket" >&2
    exit 1
  fi
  sleep 0.05
done

ctl() {
  CONTEXT_MANAGER_HOME="$home" CONTEXTD_BIN="$root/target/debug/contextd" \
    "$root/target/debug/contextctl" "$@"
}

ctl ping --json \
  | jq -e '.schema_version >= 1 and .api_version == 1 and (.component_version | length > 0)' \
  >/dev/null

ctl doctor --json >"$workdir/doctor.json"
jq -e '
  any(.checks[]; .id == "daemon_reachable" and .status == "pass")
  and any(.checks[]; .id == "mcp_handshake" and .status == "pass")
  and any(.checks[]; .id == "review_policy" and .status == "pass")
' "$workdir/doctor.json" >/dev/null

printf '# Project instructions\n\nKeep setup checks deterministic.\n' >"$project/AGENTS.md"
ctl setup --project "$project" --source "$project/AGENTS.md" --json >"$workdir/setup.json"
jq -e '
  .project_scope.kind == "project"
  and .import_preview.candidates[0].detected_source_kind == "agents_md"
  and .import_result == null
' "$workdir/setup.json" >/dev/null

ctl source-import apply "$project/AGENTS.md" \
  --scope project \
  --scope-id "$project" \
  --json >"$workdir/source-import.json"
jq -e '
  .candidate_count == 1
  and .applied_count == 1
  and .rejected_count == 0
' "$workdir/source-import.json" >/dev/null

ctl policy set strict --actor smoke --json >"$workdir/policy-strict.json"
jq -e '.mode == "strict"' "$workdir/policy-strict.json" >/dev/null
ctl policy set balanced --actor smoke --json >"$workdir/policy-balanced.json"
jq -e '.mode == "balanced"' "$workdir/policy-balanced.json" >/dev/null

ctl entry put \
  --scope project \
  --scope-id "$project" \
  --pack main \
  --key conventions \
  --title "Project conventions" \
  --kind instruction \
  --body "Use deterministic request identifiers." \
  --tag workflow \
  --actor smoke \
  --json >"$workdir/put.json"
jq -e '.scope.kind == "project" and .key == "conventions"' "$workdir/put.json" >/dev/null

ctl compose --project "$project" --json >"$workdir/compose.json"
jq -e '
  (.rendered_markdown | contains("deterministic request identifiers"))
  and (.rendered_markdown | contains("Keep setup checks deterministic"))
  and .metrics.included_entries == 2
' "$workdir/compose.json" >/dev/null

cat >"$workdir/global-commit.json" <<'JSON'
{
  "request_id": "smoke-global-1",
  "actor": "smoke-agent",
  "run": {
    "id": "smoke-run-1",
    "project_scope_id": "SMOKE_PROJECT_REPLACED_BY_SED",
    "source": "e2e-smoke",
    "metadata": {"purpose": "acceptance-test"}
  },
  "proposals": [
    {
      "scope": {"kind": "global", "id": "global"},
      "pack_name": "main",
      "entry": {
        "key": "preferred-output",
        "title": "Preferred output",
        "kind": "preference",
        "format": "markdown",
        "body": "Prefer concise implementation summaries.",
        "tags": ["communication"],
        "metadata": {},
        "locked": false
      }
    }
  ]
}
JSON
PROJECT_PATH="$project" python3 - "$workdir/global-commit.json" <<'PY'
import json, os, pathlib, sys
path = pathlib.Path(sys.argv[1])
payload = json.loads(path.read_text())
payload["run"]["project_scope_id"] = os.environ["PROJECT_PATH"]
path.write_text(json.dumps(payload))
PY

ctl commit-work --file "$workdir/global-commit.json" --json >"$workdir/commit.json"
jq -e '.status == "pending" and .items[0].disposition == "pending"' "$workdir/commit.json" >/dev/null
review_id="$(jq -er '.items[0].review_id' "$workdir/commit.json")"

# Unfiltered review listing is the normal UI/CLI path and must accept an absent state.
ctl review list --json >"$workdir/reviews.json"
jq -e --arg id "$review_id" 'any(.[]; .id == $id and .state == "pending")' "$workdir/reviews.json" >/dev/null
ctl review approve "$review_id" --actor smoke-reviewer --note "accepted by smoke test" --json \
  | jq -e '.state == "approved"' >/dev/null

ctl compose --project "$project" --json >"$workdir/composed-after-review.json"
jq -e '.rendered_markdown | contains("Prefer concise implementation summaries")' \
  "$workdir/composed-after-review.json" >/dev/null

# Exercise the built MCP server through the exact plugin launcher used by a harness.
cat >"$workdir/mcp-input.jsonl" <<'JSONL'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e-smoke","version":"1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
JSONL
jq -nc --arg project "$project" '{
  jsonrpc: "2.0",
  id: 3,
  method: "tools/call",
  params: {
    name: "commit_work",
    arguments: {
      request_id: "smoke-mcp-write-1",
      actor: "codex-smoke-agent",
      run: {
        id: "smoke-mcp-run-1",
        project_scope_id: $project,
        source: "codex",
        metadata: {purpose: "adapter-acceptance-test"}
      },
      proposals: [{
        scope: {kind: "project", id: $project},
        pack_name: "main",
        entry: {
          key: "mcp-handoff",
          title: "MCP handoff",
          kind: "handoff",
          format: "markdown",
          body: "The real MCP adapter completed its post-work write.",
          tags: ["handoff"],
          metadata: {},
          locked: false
        }
      }]
    }
  }
}' >>"$workdir/mcp-input.jsonl"
CONTEXT_MANAGER_HOME="$home" \
CONTEXT_MCP_BIN="$root/target/debug/context-mcp" \
CONTEXT_MANAGER_HARNESS="codex" \
  sh plugins/context-manager/scripts/run-context-mcp.sh \
  <"$workdir/mcp-input.jsonl" >"$workdir/mcp-output.jsonl"
jq -s -e 'length == 3 and ((.[1].result.tools | map(.name) | sort) == ["commit_work", "compose_context", "search_context"]) and .[2].result.structuredContent.status == "applied"' \
  "$workdir/mcp-output.jsonl" >/dev/null
ctl compose --project "$project" --json \
  | jq -e '.rendered_markdown | contains("real MCP adapter completed its post-work write")' >/dev/null

# Exercise the real hook wrapper and verify the shared Codex/Claude JSON contract.
printf '%s\n' "{\"session_id\":\"smoke-hook-run\",\"cwd\":\"$project\"}" \
  | CONTEXT_MANAGER_HOME="$home" \
    CONTEXTCTL_BIN="$root/target/debug/contextctl" \
    sh plugins/context-manager/scripts/run-context-hook.sh codex session-start \
    >"$workdir/hook-output.json"
jq -e '.hookSpecificOutput.hookEventName == "SessionStart"' "$workdir/hook-output.json" >/dev/null
jq -e '.hookSpecificOutput.additionalContext | contains("deterministic request identifiers") and contains("commit_work")' \
  "$workdir/hook-output.json" >/dev/null

printf '%s\n' "{\"session_id\":\"smoke-hook-run\",\"cwd\":\"$project\"}" \
  | CONTEXT_MANAGER_HOME="$home" \
    CONTEXTCTL_BIN="$root/target/debug/contextctl" \
    sh plugins/context-manager/scripts/run-context-hook.sh codex session-end \
    >"$workdir/session-end.out"
test ! -s "$workdir/session-end.out"

echo "end-to-end smoke test passed"
