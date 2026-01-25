# Infra

Infra is a production‑grade **stdio MCP server** for AI‑agent operations. It provides a single, deterministic interface to SSH, HTTP, Postgres, git/repo ops, pipelines, runbooks, intents, evidence, audit, and state.

## Quickstart (2 minutes)

1) Install
- Download a prebuilt binary from GitHub Releases.
- Or build from source:

```bash
cargo build --release
# binary: target/release/infra
```

2) Configure your MCP client (example shape; adjust for your client)

```json
{
  "mcpServers": {
    "infra": {
      "command": "/path/to/infra",
      "args": [],
      "env": {
        "MCP_PROFILES_DIR": "/path/to/your/project/.infra"
      }
    }
  }
}
```

3) Sanity check

```json
{ "tool": "help", "args": { "query": "runbook" } }
```

## Client configs

### Claude Desktop
Common config locations:
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Linux: `~/.config/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\\Claude\\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "infra": {
      "command": "/path/to/infra",
      "args": [],
      "env": {
        "MCP_PROFILES_DIR": "/path/to/your/project/.infra"
      }
    }
  }
}
```

### VS Code
Create `.vscode/mcp.json` in your workspace:

```json
{
  "servers": {
    "infra": {
      "type": "stdio",
      "command": "/path/to/infra",
      "args": [],
      "env": {
        "MCP_PROFILES_DIR": "/path/to/your/project/.infra"
      }
    }
  }
}
```

### Zed
Add to your Zed `settings.json`:

```json
{
  "context_servers": {
    "infra": {
      "command": "/path/to/infra",
      "args": [],
      "env": {
        "MCP_PROFILES_DIR": "/path/to/your/project/.infra"
      }
    }
  }
}
```

## What Infra gives you
- Deterministic, auditable infrastructure actions (audit + evidence + artifacts).
- Repeatable workflows via runbooks and intents.
- Safe‑by‑default execution with explicit opt‑ins for risky operations.
- Project‑isolated state so agents don’t leak configs across repos.

## Project isolation (recommended)
Set a per‑repo profiles directory:

```
MCP_PROFILES_DIR=/path/to/your/project/.infra
```

Optional explicit paths:
- `MCP_RUNBOOKS_PATH=/path/to/your/project/.infra/runbooks.json`
- `MCP_CAPABILITIES_PATH=/path/to/your/project/.infra/capabilities.json`
- `MCP_CONTEXT_REPO_ROOT=/path/to/your/project/.infra/artifacts`

## Tool discovery
Infra exposes a rich tool catalog. Use these to discover exact schemas and actions:

```json
{ "tool": "help", "args": { "query": "ssh exec" } }
```

Machine‑readable catalog:
- `tool_catalog.json`

Stdin options (for `ssh`, `env`, `repo`, `mcp_local`):
- `stdin`: plain text
- `stdin_base64`: binary input
- `stdin_file`: stream from local file
- `stdin_ref`: stream from artifact (`artifact://...`)
- `stdin_eof`: control EOF behavior (default: true)

## Common operations (examples)
List runbooks:

```json
{ "tool": "runbook", "args": { "action": "list", "query": "k8s", "limit": 20 } }
```

Run a runbook:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "k8s.diff", "input": { "overlay": "./overlays/dev" } } }
```

Run a remote command:

```json
{ "tool": "ssh", "args": { "action": "exec", "target": "prod", "command": "uptime" } }
```

Make an HTTP request:

```json
{ "tool": "http", "args": { "action": "request", "method": "GET", "url": "https://example.com/health" } }
```

Query Postgres:

```json
{ "tool": "psql", "args": { "action": "query", "sql": "select now()" } }
```

Note: For exact tool names and schemas, use `help` or `tool_catalog.json`.

## Recipes

### GitOps: kustomize diff (requires `INFRA_UNSAFE_LOCAL=1` + kubectl)
Define the runbook:

```json
{
  "tool": "runbook",
  "args": {
    "action": "upsert",
    "name": "gitops.k8s.diff",
    "runbook": {
      "description": "Render kustomize overlay and diff against the cluster.",
      "tags": ["gitops", "k8s", "read"],
      "inputs": ["overlay", "kubeconfig"],
      "steps": [
        {
          "id": "render",
          "tool": "mcp_local",
          "args": {
            "action": "exec",
            "command": "kubectl",
            "args": ["kustomize", "{{ input.overlay }}"],
            "env": { "KUBECONFIG": "{{ input.kubeconfig }}" },
            "inline": true
          }
        },
        {
          "id": "diff",
          "tool": "mcp_local",
          "args": {
            "action": "exec",
            "command": "kubectl",
            "args": ["diff", "-f", "-"],
            "stdin": "{{ steps.render.stdout }}",
            "env": { "KUBECONFIG": "{{ input.kubeconfig }}" },
            "inline": true
          }
        }
      ]
    }
  }
}
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "gitops.k8s.diff", "input": { "overlay": "./overlays/dev", "kubeconfig": "~/.kube/config" } } }
```

### VPS: restart a service over SSH
Define the runbook:

```json
{
  "tool": "runbook",
  "args": {
    "action": "upsert",
    "name": "vps.service.restart",
    "runbook": {
      "description": "Restart a systemd service and check status.",
      "tags": ["vps", "ssh", "write"],
      "inputs": ["target", "service"],
      "steps": [
        {
          "id": "restart",
          "tool": "ssh",
          "args": {
            "action": "exec",
            "target": "{{ input.target }}",
            "command": "sudo systemctl restart {{ input.service }}"
          }
        },
        {
          "id": "status",
          "tool": "ssh",
          "args": {
            "action": "exec",
            "target": "{{ input.target }}",
            "command": "systemctl status {{ input.service }} --no-pager"
          }
        }
      ]
    }
  }
}
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "vps.service.restart", "input": { "target": "prod", "service": "nginx" } } }
```

### DB: export a table to CSV
Define the runbook:

```json
{
  "tool": "runbook",
  "args": {
    "action": "upsert",
    "name": "db.export.table",
    "runbook": {
      "description": "Export a table to CSV on the Infra host.",
      "tags": ["db", "backup", "read"],
      "inputs": ["profile_name", "table", "file_path"],
      "steps": [
        {
          "id": "export",
          "tool": "psql",
          "args": {
            "action": "export",
            "profile_name": "{{ input.profile_name }}",
            "table": "{{ input.table }}",
            "file_path": "{{ input.file_path }}",
            "format": "csv",
            "csv_header": true
          }
        }
      ]
    }
  }
}
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "db.export.table", "input": { "profile_name": "prod-db", "table": "events", "file_path": "/var/backups/events.csv" } } }
```

## Safety & scope
- Local exec + filesystem tools are disabled unless `INFRA_UNSAFE_LOCAL=1`.
- Secret export is disabled unless `INFRA_ALLOW_SECRET_EXPORT=1`.
- Use `timeout_ms` and limit list sizes for reliability.
- Prefer read‑only actions unless a write is explicit.

## Troubleshooting
- If a tool call times out, increase your client timeout or reduce batch size.
- Use audit/evidence tools to inspect what happened.
- Run `./tools/doctor` for diagnostics when building from source.

## Documentation
- `mcp_config.md` — MCP client configuration
- `docs/RUNBOOK.md` — runbooks
- `docs/INTEGRATION.md` — integration checks
- `SECURITY.md` — security policy

## For contributors
- `./tools/doctor` — diagnostics
- `./tools/gate` — fmt + clippy + tests
