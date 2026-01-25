 [LEGEND]
# Uses repo-wide tokens from LEGEND.md.

 [CONTENT]
This codebase is organized around explicit seams ([BOUNDARY]) and a deterministic execution core.

**Execution path**
- [ENTRYPOINT]: `src/main.rs` runs `infra::mcp::server::run_stdio()` (stdio MCP).
- [MCP_SERVER]: `src/mcp/server.rs` validates + routes tool calls.
- [APP_WIRING]: `src/app.rs` constructs services and managers, then builds the [TOOL_EXECUTOR].

**Core components**
- [TOOL_EXECUTOR] (`src/services/tool_executor.rs`): single place for alias/preset merge + result wrapping + audit.
- [RUNBOOK_ENGINE] (`src/managers/runbook.rs`): executes `runbooks.json` steps via the [TOOL_EXECUTOR].
- [INTENT_ENGINE] (`src/managers/intent.rs`): compiles intents using `capabilities.json`, then runs runbooks via the [RUNBOOK_ENGINE].
- [PIPELINE_ENGINE] (`src/managers/pipeline/`): streams between HTTP/SFTP/Postgres with optional artifact capture.

**State + config**
- Profiles/state/projects default to an XDG state dir, or can be isolated via `MCP_PROFILES_DIR`.
- `runbooks.json` and `capabilities.json` are loaded as defaults; “local” overrides live under the profile dir.
