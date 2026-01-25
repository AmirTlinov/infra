 [LEGEND]
SRC_LAYOUT = The canonical Rust module layout for this repo.
NEW_TOOL_FLOW = The end-to-end steps to add a new MCP tool safely.
WIRING_ONLY = Rule: `src/app.rs` is wiring only (no new business logic).

 [CONTENT]
This file documents the structural rules that keep the codebase predictable for humans and AI agents.

## Layout ([SRC_LAYOUT])
- `src/mcp/`: stdio MCP protocol server, request parsing, routing, result envelope
- `src/app.rs`: dependency graph ([APP_WIRING]) and tool registry
- `src/services/`: stateful building blocks ([SERVICE]) shared across tools
- `src/managers/`: tool handlers ([MANAGER]) implementing the MCP surface
- `src/utils/`: mostly-pure helpers (prefer small, composable units)

## Add a tool ([NEW_TOOL_FLOW])
1) Create a new manager in `src/managers/<tool>.rs` (or `src/managers/<tool>/mod.rs` when it grows).
2) Implement `ToolHandler` by delegating to `handle_action` (keep argument validation inside the manager).
3) Wire it in `src/app.rs` handlers map (and update builtin aliases in `src/mcp/aliases.rs` if needed).
4) Add/extend focused tests, then run the [GATE] (`./tools/gate`).

## Wiring rule ([WIRING_ONLY])
- Put dependency wiring in `src/app.rs`.
- Put behavior and rules in managers/services.
- If a change adds a new responsibility, it gets a new module owner (no “misc/utils/common2”).
