   [LEGEND]
DEBUG_LOGS = Turning on debug logs for investigation.
STATE_DIR = The directory where local state/config files live.
FAIL_CLOSED = A gate behavior: stop on drift, don’t “best-effort” correctness.
EFFECTS = Side-effect metadata for an action: `{ kind, requires_apply, irreversible }`.
APPLY = The `apply=true` opt-in flag required for write/mixed [EFFECTS].
CONFIRM = The `confirm=true` explicit acknowledgement required for irreversible [EFFECTS].
RUNBOOK_MANIFEST = The file-backed `runbooks.json` manifest loaded from `MCP_RUNBOOKS_PATH` or `MCP_PROFILES_DIR`.

 [CONTENT]
## Debugging

- [DEBUG_LOGS]: set `LOG_LEVEL=debug` to see tool-level debug logs on stderr.
- Errors are structured as `ToolError` (kind + code + message + optional hint/details).

## Local state

- [STATE_DIR] defaults to an XDG state dir (for example `~/.local/state/infra`).
- Set `MCP_PROFILES_DIR=/path/to/dir` to fully isolate profiles/state/projects/runbooks/capabilities.
- Normal-mode runbook execution is manifest-backed from [RUNBOOK_MANIFEST]; edit that file instead of trying to mutate runbooks through the runtime API.

## Determinism

- [FAIL_CLOSED]: run `./tools/gate-code` for engineering verification, `./tools/gate-docs` for doc/contract hygiene, then `./tools/gate` before shipping changes.
- For docs, follow `docs/DOC_STYLE.md` and keep meanings in `LEGEND.md`.

## Safety (effects + confirmation)

- Tools that execute workflows (workspace / intent / runbook) attach [EFFECTS].
- Inline runbook payloads, runbook DSL execution/compile, and mutable runbook upsert/delete actions are compatibility-only in normal mode.
- If `effects.requires_apply=true`, you must pass [APPLY] to execute.
- If `effects.irreversible=true`, you must also pass [CONFIRM].

See `docs/RECIPES.md` for copy/paste examples (request → expected artifact).
