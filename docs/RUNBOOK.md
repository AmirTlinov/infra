  [LEGEND]
DEBUG_LOGS = Turning on debug logs for investigation.
STATE_DIR = The directory where local state/config files live.
FAIL_CLOSED = A gate behavior: stop on drift, don’t “best-effort” correctness.

 [CONTENT]
## Debugging

- [DEBUG_LOGS]: set `LOG_LEVEL=debug` to see tool-level debug logs on stderr.
- Errors are structured as `ToolError` (kind + code + message + optional hint/details).

## Local state

- [STATE_DIR] defaults to an XDG state dir (for example `~/.local/state/infra`).
- Set `MCP_PROFILES_DIR=/path/to/dir` to fully isolate profiles/state/projects/runbooks/capabilities.

## Determinism

- [FAIL_CLOSED]: always run `./tools/gate` before shipping changes.
- For docs, follow `docs/DOC_STYLE.md` and keep meanings in `LEGEND.md`.
