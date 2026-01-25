[LEGEND]
DOC_FORMAT = The canonical doc shape: a `[LEGEND]` block then a `[CONTENT]` block.
LEGEND_BLOCK = The `[LEGEND]` block containing definitions.
CONTENT_BLOCK = The `[CONTENT]` block containing the document body.
TOKEN = A named meaning reused across docs.
GLOBAL_TOKEN = A token defined in `LEGEND.md`; available repo-wide.
LOCAL_TOKEN = A token defined in a specific doc; scoped to that doc.
TOKEN_REF = A reference in content like `[TOKEN]` (optionally `[TOKEN|LEGEND.md]`).
NO_SHADOWING = Rule: a doc must not redefine a global token locally.
GATE = A deterministic checker that fails closed on drift.
DOCTOR = A diagnostic checker for environment + repo foundation.
CONTRACT = A versioned interface spec with examples.
CHANGE_PROTOCOL = The sequence: contracts → implementation → tests → docs.
BOUNDARY = A seam where we define contracts to prevent implicit coupling.
ENTRYPOINT = The process entry that starts the stdio MCP server.
MCP_SERVER = The stdio server that parses requests and routes tool calls.
APP_WIRING = The dependency graph assembly (construct services/managers, connect them).
MANAGER = A tool-facing handler (implements ToolHandler; validates args; orchestrates services).
SERVICE = A stateful or reusable component (profiles/state/policy/cache/etc).
TOOL_EXECUTOR = The core dispatcher: resolves aliases/presets, executes tools, wraps results, audits.
RUNBOOK_ENGINE = The runbook runner that executes a sequence of tool calls with templating and state.
INTENT_ENGINE = The intent compiler/executor that maps an intent type to a runbook plan.
PIPELINE_ENGINE = The streaming data mover between HTTP/SFTP/Postgres with optional artifact capture.

[CONTENT]
This file is the global vocabulary for the repo.

Use it when:
- A meaning repeats across multiple documents.
- You want agents to reuse the same mental model without re-parsing prose.

Avoid it when:
- The concept is unique to one doc (keep it local).
