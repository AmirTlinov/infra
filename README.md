# Infra

Infra is a **stdio MCP server** that exposes ops-grade tools to AI agents:
SSH, HTTP client, Postgres, runbooks, pipelines, intents, artifacts, state, and more.

This repository is the Rust implementation, optimized for:
- High throughput + low overhead
- Deterministic agent-friendly interfaces
- Fail-closed development gates + AI-native documentation

## Quick start (dev)

1) Diagnose:

`./tools/doctor`

2) Run all gates (fmt + clippy + tests):

`./tools/gate`

## AI DX

- Common action aliases are accepted (list/get/delete/run/upsert/use), normalized server-side.
- List actions support limit/offset/query/where; tag filters for runbooks + capabilities; list responses include meta.
- `help` now surfaces action aliases and concrete examples.

## Docs

- `mcp_config.md` (MCP client configuration)
- `docs/INTEGRATION.md` (integration smoke and local stack)
- `SECURITY.md` (vulnerability reporting)
- `PUBLIC_RELEASE_CHECKLIST.md` (release hygiene)

## Parity (TS → Rust)

If you still have the legacy TypeScript repo available locally, you can verify
deterministic parity between the TS and Rust servers (default `--suite extended`):

`./tools/parity --ts-path /path/to/legacy-ts`

Notes:
- `./tools/parity` runs both servers with isolated temp state (`MCP_PROFILES_DIR`) and isolated temp context roots (`INFRA_CONTEXT_REPO_ROOT`).
- Use `--suite core` for a faster “surface sanity check”.

## Smoke (docker)

Optional but high-signal end-to-end smoke (starts ephemeral Postgres + SSH containers, plus a local HTTP server):

`./tools/smoke`

## Run (stdio MCP)

Dev:

`cargo run`

Release:

`cargo run --release`

## Key files

- `src/main.rs`: stdio entrypoint
- `src/mcp/server.rs`: MCP protocol server + routing
- `src/app.rs`: wiring (DI) of managers/services
- `runbooks.json`: default runbooks
- `capabilities.json`: default intent capabilities
- `tool_catalog.json`: tool catalog (schema-ish)

## Config (high-signal)

- `LOG_LEVEL=debug` enables debug logs
- `MCP_PROFILES_DIR=...` isolates all local state/config into a directory
- `INFRA_UNSAFE_LOCAL=1` enables `mcp_local` (disabled by default)
- `INFRA_ALLOW_SECRET_EXPORT=1` allows `include_secrets=true` on profile export actions
