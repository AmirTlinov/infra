# Infra

Infra is a **stdio MCP server** for AI agents that need to do real operational work — not just talk about it.

It gives your MCP client one place to **inspect servers over SSH**, **call APIs**, **query Postgres**, **understand repos**, **run repeatable runbooks**, and **save artifacts/audit trails** of what happened.

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)

## What is it?

If you use Claude, Codex, VS Code, Zed, or another MCP client, Infra is the server you plug in when you want the model to do infrastructure and delivery work in a structured way.

Instead of giving an agent a pile of scripts or broad shell access, Infra gives it:

- one MCP endpoint,
- a predictable tool surface,
- reusable runbooks for recurring tasks,
- project-local state,
- and evidence for what happened.

In practice, that means the model can do useful work like:

- checking whether a service is healthy,
- querying a database,
- inspecting a repo before a change,
- running a deployment workflow,
- or collecting evidence during an incident.

## Why is it useful?

Infra is useful when you want an AI agent to be genuinely operationally helpful, but still understandable and reviewable by humans.

- **It turns vague agent requests into concrete operations** — instead of “figure it out in shell”, the model gets real tools for SSH, HTTP, Postgres, repo workflows, and runbooks.
- **It makes repeatable work actually repeatable** — once a workflow exists as a runbook, the agent can reuse it instead of improvising every time.
- **It gives you proof, not just claims** — artifacts and audit trails make it easier to inspect outputs, share evidence, and answer “what exactly happened?”
- **It is safer than default shell-first setups** — risky local execution and secret export are off unless you explicitly enable them.
- **It reduces tool sprawl** — one MCP server can cover a big chunk of day-to-day ops and automation work.

## What kinds of jobs is it good at?

Infra shines when you want prompts like these to turn into reliable actions:

- “Check prod health and show me where the failure starts.”
- “Snapshot this repo and summarize what changed.”
- “Run the staging deploy workflow.”
- “Query Postgres and export the result.”
- “Use the existing runbook instead of inventing a shell script.”

## Install

### 1. Build Infra

Prerequisite: a Rust toolchain.

```bash
cargo build --release
```

Binary path:

```bash
target/release/infra
```

If your platform is published on [GitHub Releases](https://github.com/AmirTlinov/infra/releases), you can use a prebuilt binary instead.

### 2. Create a project profile directory (recommended)

Infra works best when each project has its own state and manifests.

```bash
mkdir -p .infra
export MCP_PROFILES_DIR="$PWD/.infra"
```

Infra will use this directory for project-local state such as runbooks, capabilities, and the SQLite store.

### 3. Add Infra to your MCP client

Example config shape:

```json
{
  "mcpServers": {
    "infra": {
      "command": "/absolute/path/to/infra/target/release/infra",
      "args": [],
      "env": {
        "MCP_PROFILES_DIR": "/absolute/path/to/your/project/.infra"
      }
    }
  }
}
```

> Adjust the JSON shape to match your client. The important parts are the `command` and `MCP_PROFILES_DIR`.

### 4. Sanity check

Once your client starts Infra, try one of these:

```json
{ "tool": "help", "args": { "query": "runbook" } }
```

```json
{ "tool": "runbook", "args": { "action": "run", "name": "repo.snapshot", "input": { "repo_path": "." } } }
```

The first call shows you what Infra can do. The second runs a simple read-only snapshot of the current repo.

## Safe defaults

Infra is intentionally locked down by default.

| Capability | Env var | Default |
|---|---|---|
| Local shell/filesystem access | `INFRA_UNSAFE_LOCAL=1` | off |
| Secret export | `INFRA_ALLOW_SECRET_EXPORT=1` | off |

That means you can start with a conservative setup and only opt into risky capabilities when you actually need them.

## Want to go further?

- `docs/RECIPES.md` — copy/paste examples for common workflows
- `docs/INTEGRATION.md` — local smoke test
- `docs/contracts/README.md` — lower-level contracts and formats

## License

Apache-2.0
