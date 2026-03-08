 [LEGEND]
# Uses repo-wide tokens from LEGEND.md.

 [CONTENT]
This codebase is organized around explicit seams ([BOUNDARY]) and a deterministic execution core.

**Execution path**
- [ENTRYPOINT]: `src/main.rs` runs `infra::mcp::server::run_stdio()` (stdio MCP).
- [MCP_SERVER]: `src/mcp/server.rs` validates + routes tool calls.
- [APP_WIRING]: `src/app.rs` constructs services and managers, then builds the [TOOL_EXECUTOR].

**Core components**
- [TOOL_EXECUTOR] (`src/services/tool_executor.rs`): single place for alias-compat resolution + result wrapping + audit; preset merges are rejected as compatibility-only.
- [RUNBOOK_ENGINE] (`src/managers/runbook.rs`): executes `runbooks.json` steps via the [TOOL_EXECUTOR].
- [INTENT_ENGINE] (`src/managers/intent.rs`): compiles intents using `capabilities.json`, then runs runbooks via the [RUNBOOK_ENGINE].
- [PIPELINE_ENGINE] (`src/managers/pipeline/`): streams between HTTP/SFTP/Postgres with optional artifact capture.
- `mcp_operation` is the capability-first kernel entrypoint for observe/plan/apply/verify/rollback/status/cancel flows, with typed receipts stored in the shared SQLite-backed operation state.

**State + config**
- Profiles/state/projects default to an XDG state dir, or can be isolated via `MCP_PROFILES_DIR`.
- Mutable **operational** records now live in a single SQLite store (`infra.db`, override via `MCP_STORE_DB_PATH`); legacy JSON files are import-only compatibility sources.
- `runbooks.json` and `capabilities.json` stay file-backed defaults/manifests. Legacy mutable overlays still exist for compatibility, but the redesign direction is to keep capability semantics/versioned recipes out of the operational store hot path.
- `tools/list` defaults to a low-entropy core tier; the broader canonical expert plane is opt-in via `INFRA_TOOL_TIER=expert`.
