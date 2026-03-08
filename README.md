# Infra

Infra is a production‑grade **stdio MCP server** for AI‑agent operations. It provides a single, deterministic interface to SSH, HTTP, Postgres, git/repo ops, pipelines, runbooks, intents, evidence, audit, and state.

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

## Why Infra? What's different?

Most MCP tool servers give agents raw shell access and hope for the best. **Infra takes the opposite approach**: every action goes through a unified interface with built-in audit, evidence, and explicit opt-ins for risky ops.

- **One server, full stack** — SSH, HTTP, Postgres, git, runbooks, state. No juggling 5 different MCP servers.  
- **Audit by default** — Every call is logged with evidence/artifacts. You can always answer "what did the agent do?"  
- **Safe-by-default execution** — Local exec and secret export are *disabled* unless you explicitly opt in. Agents can't escape the sandbox by accident.  
- **Transactional store** — Mutable server state lives in a single SQLite store instead of whole-file JSON rewrites.  
- **MCP-native context** — Canonical tools stay small; inventory/help surfaces are also exposed through MCP resources/prompts.  
- **Low-entropy default surface** — `tools/list` defaults to the capability-first core surface; the wider expert plane is opt-in via `INFRA_TOOL_TIER=expert`.  

Core engineering idea: **deterministic, auditable infrastructure actions** with the minimal surface area an agent actually needs.

## Safe-by-default

Infra is locked down out of the box. Risky capabilities require explicit environment variables:

| Capability | Env var | Default |
|------------|---------|---------|
| Local shell/filesystem access | `INFRA_UNSAFE_LOCAL=1` | **disabled** |
| Secret export (env vars, tokens) | `INFRA_ALLOW_SECRET_EXPORT=1` | **disabled** |

Without these flags, agents cannot execute arbitrary local commands or leak secrets — even if they try.

Infra also bounds leaked stdio sessions by default:

| Lifecycle guard | Env var | Default |
|-----------------|---------|---------|
| Auto-exit idle stdio MCP server | `INFRA_STDIO_IDLE_TIMEOUT_MS` | `30000` ms |

Set `INFRA_STDIO_IDLE_TIMEOUT_MS=0` to disable the idle self-shutdown, but the default is recommended when many CLI sessions may otherwise leave orphaned MCP child processes behind.
The idle timer only applies while Infra is waiting for the next stdio JSON-RPC line; an in-flight request/tool call is allowed to finish normally before the process exits.

## Quick demo (30 seconds)

```text
You: "What runbooks do we have for deploys?"
Agent → help { query: "deploy runbook" }
      ← Found: deploy.k8s, deploy.staging, deploy.rollback ...

You: "Run staging deploy for service 'api'"
Agent → runbook { action: "run", name: "deploy.staging", input: { service: "api" } }
      ← ✓ Artifacts: artifact://runs/deploy.staging/2026-01-29T10:30:00Z
        Evidence: commit abc123 deployed to staging-api-7f8d9, health-check passed.
```

Every run creates an artifact with full audit trail — what ran, what changed, what the output was.

## Quickstart (2 minutes)

1) Install

- Download a prebuilt binary from [GitHub Releases](https://github.com/AmirTlinov/infra/releases).
- Or build from source:

```bash
cargo build --release
# binary: target/release/infra
```

1) Configure your MCP client (example shape; adjust for your client)

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

1) Sanity check

```json
{ "tool": "help", "args": { "query": "runbook" } }
```

1) Baseline snapshot (optional, but recommended before major redesign work)

```bash
./tools/baseline --out .artifacts/baseline/current.json
```

This writes a generated snapshot of the **current checkout at execution time** (tool inventory, capabilities/runbooks inventory, versions, dirty diff, and gate entrypoints) so architecture docs do not need to embed stale “facts of today”.

Generated local proof outputs under `.artifacts/` are gitignored to keep the working tree clean.

## First run in 60 seconds

No external services needed — just run a built-in runbook to see Infra in action:

```json
// 1. List available runbooks
{ "tool": "runbook", "args": { "action": "list", "query": "repo", "limit": 5 } }

// 2. Run repo.snapshot on the current directory (read-only, no side effects)
{ "tool": "runbook", "args": { "action": "run", "name": "repo.snapshot", "input": { "repo_path": "." } } }
```

Output includes: repo root, current branch, recent commits, diffstat — and an `artifact://` reference you can browse later.

## Verify checksum

When downloading from [Releases](https://github.com/AmirTlinov/infra/releases), verify the binary:

```bash
# Download binary and checksum
curl -LO https://github.com/AmirTlinov/infra/releases/latest/download/infra-linux-x86_64
curl -LO https://github.com/AmirTlinov/infra/releases/latest/download/infra-linux-x86_64.sha256

# Verify
sha256sum -c infra-linux-x86_64.sha256
chmod +x infra-linux-x86_64
./infra-linux-x86_64
```

> **Note**: Prebuilt binaries are available starting with future releases. For now, build from source with `cargo build --release`.

## Project templates

Create a minimal `.infra/` directory for your project:

```bash
mkdir -p .infra
```

**.infra/runbooks.json** (minimal):

```json
{
  "hello.world": {
    "description": "A simple test runbook.",
    "tags": ["test"],
    "inputs": ["message"],
    "steps": [
      {
        "id": "echo",
        "tool": "mcp_state",
        "args": { "action": "set", "key": "hello", "value": "{{ input.message }}" }
      }
    ]
  }
}
```

**.infra/capabilities.json** (minimal):

```json
{
  "version": 1,
  "capabilities": {}
}
```

**.infra/targets.json** (example SSH target):

```json
{
  "prod": {
    "host": "prod.example.com",
    "port": 22,
    "username": "deploy",
    "key_file": "~/.ssh/id_ed25519"
  }
}
```

Then point Infra to your project:

```bash
export MCP_PROFILES_DIR=/path/to/your/project/.infra
```

Infra now uses a **single SQLite store** for mutable operational state under the profile base dir:

- default path: `MCP_PROFILES_DIR/infra.db`
- override: `MCP_STORE_DB_PATH=/custom/path/infra.db`

Legacy JSON files such as `profiles.json`, `state.json`, `projects.json`, `aliases.json`, `presets.json`, and `jobs.json` are treated as **one-time import sources** for operational/local state, not the canonical writable store.

`runbooks.json` and `capabilities.json` remain **file-backed manifests/defaults**. In normal mode, runbooks are resolved directly from these manifests: `runbook_run` is name-only and manifest-backed, while inline runbook payloads plus `runbook_upsert`, `runbook_upsert_dsl`, `runbook_delete`, `runbook_run_dsl`, and `runbook_compile` are compatibility-only migration paths that are intentionally rejected.

Derived context is no longer persisted as writable truth: Infra computes it on demand and may reuse only a **process-local session cache** until `refresh=true`.

## Tool surface tiers

Infra now has two discovery modes:

- **Default (`INFRA_TOOL_TIER=core`)** — low-entropy capability kernel surface (`help`, `legend`, `mcp_capability`, `mcp_operation`, `mcp_receipt`, `mcp_policy`, `mcp_profile`, `mcp_target`). Capability discovery is read-focused here; mutable compatibility actions stay off the main machine surface.
- **Expert (`INFRA_TOOL_TIER=expert`)** — wider canonical surface for raw/debug/legacy flows (SSH, HTTP, SQL, runbook, audit, alias, preset compatibility storage, etc).

Builtin aliases remain compatibility shims for explicit `tools/call`, but they are hidden from `tools/list`.

## Browsing artifacts

Every tool call can produce artifacts. Here's how to browse them:

```json
// List recent artifacts
{ "tool": "mcp_artifacts", "args": { "action": "list", "limit": 10 } }

// Read a specific artifact
{ "tool": "mcp_artifacts", "args": { "action": "get", "uri": "artifact://runs/repo.snapshot/2026-01-29T10:00:00Z", "max_bytes": 4096 } }

// Get just the tail (last N bytes)
{ "tool": "mcp_artifacts", "args": { "action": "tail", "uri": "artifact://...", "max_bytes": 1024 } }
```

Artifacts contain: full command output, timing, exit codes, and any structured data returned by the tool.

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
- Project‑isolated state so agents don't leak configs across repos.

### Capabilities at a glance

| Category | What you can do | Example |
|----------|-----------------|---------|
| **SSH** | Execute commands, health checks, system info | `ssh { action: "exec", target: "prod", command: "uptime" }` |
| **Postgres** | Query, export tables to CSV | `psql { action: "query", sql: "SELECT now()" }` |
| **HTTP** | API requests, health checks | `http { action: "request", url: "https://api/health" }` |
| **K8s** | Render, diff, apply, rollout inspect | `runbook { name: "k8s.diff", input: { overlay: "./dev" } }` |
| **GitOps** | Full release cycle (ArgoCD/Flux) | See GitOps Autopilot below |
| **Runbooks** | Composable multi-step workflows | Chain SSH → DB → HTTP in one call |
| **Artifacts** | Store and retrieve run outputs | `artifact://runs/deploy/2026-01-29T10:30:00Z` |
| **Jobs** | Async execution, status, logs, cancel | Long-running tasks don't block |
| **Context** | Auto-detect repo type, k8s, flux/argocd | Agent knows what tools apply |
| **Preflight** | Self-diagnostics before running | "Can I connect to this cluster?" |

### Architecture

```text
┌─────────┐      stdio/JSON-RPC       ┌──────────────────────────────────────┐
│  Agent  │ ◀──────────────────────▶  │              Infra                   │
└─────────┘                           │                                      │
                                      │  ┌──────┐ ┌────────┐ ┌──────┐        │
                                      │  │ SSH  │ │Postgres│ │ HTTP │  ...   │
                                      │  └──┬───┘ └───┬────┘ └──┬───┘        │
                                      │     │         │         │            │
                                      │     ▼         ▼         ▼            │
                                      │  ┌─────────────────────────────────┐ │
                                      │  │   Audit · Evidence · Artifacts  │ │
                                      │  └─────────────────────────────────┘ │
                                      └──────────────────────────────────────┘
```

### GitOps Autopilot (ArgoCD / Flux)

Full release cycle in one runbook — no manual steps:

```text
plan → propose (branch + PR) → wait for CI → merge → sync → verify health → auto-rollback on failure
```

Built-in capabilities: `gitops.plan`, `gitops.propose`, `gitops.sync`, `gitops.verify`, `gitops.rollback`, `gitops.release`.

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

Same request with transient-channel resilience (quiet by default):

```json
{ "tool": "http", "args": { "action": "request", "method": "GET", "url": "https://example.com/health", "stability": "auto" } }
```

Query Postgres:

```json
{ "tool": "psql", "args": { "action": "query", "sql": "select now()" } }
```

Note: For exact tool names and schemas, use `help` or `tool_catalog.json`.

## Recipes

### GitOps: kustomize diff (requires `INFRA_UNSAFE_LOCAL=1` + kubectl)

Add this entry to `.infra/runbooks.json` (normal mode is manifest-backed; runtime upsert/delete/DSL paths are compatibility-only):

```json
{
  "gitops.k8s.diff": {
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
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "gitops.k8s.diff", "input": { "overlay": "./overlays/dev", "kubeconfig": "~/.kube/config" } } }
```

### VPS: restart a service over SSH

Add this entry to `.infra/runbooks.json`:

```json
{
  "vps.service.restart": {
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
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "vps.service.restart", "input": { "target": "prod", "service": "nginx" } } }
```

### DB: export a table to CSV

Add this entry to `.infra/runbooks.json`:

```json
{
  "db.export.table": {
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
```

Run it:

```json
{ "tool": "runbook", "args": { "action": "run", "name": "db.export.table", "input": { "profile_name": "prod-db", "table": "events", "file_path": "/var/backups/events.csv" } } }
```

## Troubleshooting

- If a tool call times out, increase your client timeout or reduce batch size.
- Use audit/evidence tools to inspect what happened.
- Run `./tools/doctor` for diagnostics when building from source.

## Documentation

- `mcp_config.md` — MCP client configuration
- `docs/RUNBOOK.md` — runbooks
- `docs/INTEGRATION.md` — integration checks
- `SECURITY.md` — security policy

## FAQ

**Can the agent delete my files or run arbitrary shell commands?**  
No — unless you explicitly set `INFRA_UNSAFE_LOCAL=1`. By default, local exec and filesystem access are disabled.

**Does Infra phone home or send telemetry?**  
No. Infra is fully local, stdio-only. No network calls except the ones you configure (SSH, HTTP, Postgres targets).

**What if a command hangs or takes too long?**  
Use `timeout_ms` on any tool call. For long-running tasks, use the Jobs API — async execution with status, logs, and cancel.

**What if multiple CLI sessions leak idle `infra` processes?**  
By default, an idle stdio server exits after `INFRA_STDIO_IDLE_TIMEOUT_MS` (30 seconds). This bounds leaked child-process buildup when a client forgets to reap old MCP sessions. Set the env var to `0` only if you intentionally need a long-lived pinned stdio server.

**How do I know what the agent actually did?**  
Every action creates an artifact with full audit trail. Use `mcp_artifacts { action: "list" }` to browse, or check the artifacts directory.

**Does it work with Claude Desktop / VS Code / Zed?**  
Yes — any MCP-compatible client. See [Client configs](#client-configs) for examples.

## For contributors

- `./tools/doctor` — diagnostics
- `./tools/gate-docs` — docs/contracts gate only
- `./tools/gate-code` — fmt + clippy + tests
- `./tools/gate` — full gate (`gate-docs` + `gate-code`)
- `./tools/baseline` — generated baseline snapshot for the current checkout
