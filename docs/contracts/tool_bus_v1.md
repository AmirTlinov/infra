[LEGEND]
TOOL_BUS = The contract for how tools are invoked and reported.
MCP_JSONRPC = MCP transport shape over JSON-RPC 2.0 on stdio.
ENVELOPE = The strict JSON object returned inside `tools/call` text output.
NOTIFICATION = A JSON-RPC message without an `id` (no response expected).

[CONTENT]
Contract: Tool bus v1

## Purpose
Define [TOOL_BUS]: how tools are invoked, how results are returned, and how failures are represented.

## Scope
- In scope: request/response envelopes, streaming semantics (if any), error taxonomy.
- Out of scope: tool-specific payload schemas (those are per-tool contracts).

## Interface
Infra speaks [MCP_JSONRPC] via stdio:

- Request: JSON-RPC 2.0 object with `jsonrpc`, `method`, optional `id`, and optional `params`.
- Response: JSON-RPC 2.0 object with `id` and either `result` or `error`.
- [NOTIFICATION]: when a message has no `id`, the server MUST NOT respond. (Example: `notifications/initialized`.)

Core methods:

- `initialize` → returns `protocolVersion`, `capabilities.tools.list/call`, and `serverInfo`.
- `tools/list` → returns `{"tools":[{name,description,inputSchema}, ...]}`.
- `tools/call` → returns MCP `content`:
  - `{"content":[{"type":"text","text":"<ENVELOPE as JSON string>"}]}`

The returned [ENVELOPE] is stable and machine-parsable. It includes:

- `success`: boolean
- `tool`: string (DX-friendly name, may be an alias like `ssh`)
- `action`: string|null
- `result`: any JSON (redacted/bounded)
- `duration_ms`: number|null
- `trace`: correlation ids (`trace_id`, `span_id`, `parent_span_id`)
- `artifact_uri_context`: artifact URI for the human `.context` doc (or null)
- `artifact_uri_json`: artifact URI for the machine `result.json` (or null)

## Errors
Errors are typed and fail-closed:

- JSON-RPC layer: `InvalidRequest`, `MethodNotFound`, `InvalidParams`, `InternalError`.
- Tool layer: `ToolError` carries `kind`, `code`, `retryable`, `message`, and optional `hint`.

## Examples
```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"client","version":"0"}}}
```

```json
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
```

```json
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
```

```json
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"help","arguments":{"trace_id":"run","span_id":"call"}}}
```
