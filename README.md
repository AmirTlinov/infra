# Infra

Infra is a production‑grade **stdio MCP server** for AI‑native operations: SSH, HTTP client, Postgres, runbooks, pipelines, intents, artifacts, audit, and state — fast, deterministic, and safe by default.

## Who it’s for
- Teams that need reproducible ops via MCP.
- AI agents that require a single, reliable interface to infrastructure.

## What you get
- **Ops tools**: SSH / API / SQL / Repo / Pipelines.
- **Runbooks & Intents**: orchestration for multi‑step workflows.
- **Audit & Evidence**: traceable outputs for debugging and compliance.
- **AI‑friendly DX**: strict schemas, action aliases, list filters, clear errors.

## Quick start (local)
1) Diagnose:

`./tools/doctor`

2) Run all gates (fmt + clippy + tests):

`./tools/gate`

3) Run the MCP server:

`cargo run --release`

## Project isolation (recommended)
Prevent cross‑project runbook bleed by isolating profiles per repo:

`MCP_PROFILES_DIR=/path/to/project/.infra`

Optional explicit paths:
- `MCP_RUNBOOKS_PATH=/path/to/project/.infra/runbooks.json`
- `MCP_CAPABILITIES_PATH=/path/to/project/.infra/capabilities.json`
- `MCP_CONTEXT_REPO_ROOT=/path/to/project/.infra/artifacts`

## Example calls
List runbooks:
```
{"action":"list","query":"k8s","tags":["gitops"],"limit":20}
```

Run a runbook:
```
{"action":"run","name":"k8s.diff","input":{"overlay":"./overlays/dev"}}
```

## Documentation
- `mcp_config.md` — MCP client config
- `docs/INTEGRATION.md` — integration checks
- `docs/RUNBOOK.md` — runbook guidance
- `SECURITY.md` — security policy
- `PUBLIC_RELEASE_CHECKLIST.md` — release hygiene

## Key files
- `src/main.rs` — stdio entrypoint
- `src/mcp/server.rs` — MCP routing
- `src/app.rs` — wiring (DI)
