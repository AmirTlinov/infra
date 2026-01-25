# Infra

Infra is a production‑grade **stdio MCP server** for AI‑agent operations. It provides a single, deterministic interface for SSH, HTTP, Postgres, runbooks, pipelines, intents, artifacts, audit, and state.

## What you can do
- Run safe, repeatable infrastructure actions from AI agents.
- Orchestrate multi‑step workflows with runbooks and intents.
- Store evidence/audit trails for debugging and compliance.

## How to use (as an MCP tool)
Infra runs as a stdio MCP server. Point your MCP client to the binary and call tools directly.

Example MCP config (conceptual):
```
command: infra
args: []
```

## Project isolation (important)
To avoid mixing runbooks/profiles across projects, isolate per‑repo state:

```
MCP_PROFILES_DIR=/path/to/project/.infra
```

Optional explicit paths:
- `MCP_RUNBOOKS_PATH=/path/to/project/.infra/runbooks.json`
- `MCP_CAPABILITIES_PATH=/path/to/project/.infra/capabilities.json`
- `MCP_CONTEXT_REPO_ROOT=/path/to/project/.infra/artifacts`

## Common calls
List runbooks:
```
{"action":"list","query":"k8s","tags":["gitops"],"limit":20}
```

Run a runbook:
```
{"action":"run","name":"k8s.diff","input":{"overlay":"./overlays/dev"}}
```

## Capabilities overview
- **SSH**: execute commands, batch runs, stream logs.
- **HTTP**: profile‑based API calls, pagination, downloads.
- **Postgres**: query, batch, export, schema‑safe operations.
- **Repo**: git/render/patch operations with safety gates.
- **Pipelines**: stream data between HTTP/SFTP/Postgres.
- **Runbooks/Intents**: workflow orchestration.
- **Artifacts/Audit/Evidence**: traceable outputs.

## Security notes
- Prefer read‑only actions unless explicitly required.
- Use timeouts (`timeout_ms`) and limit list sizes.
- Enable sensitive export only when necessary.

## Documentation
- `mcp_config.md` — MCP client configuration
- `docs/RUNBOOK.md` — runbook guidance
- `docs/INTEGRATION.md` — integration checks
- `SECURITY.md` — security policy

## For contributors
- `./tools/doctor` — diagnostics
- `./tools/gate` — fmt + clippy + tests
