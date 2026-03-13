[LEGEND]
INTEGRATION_SMOKE = An end-to-end sanity run using local Docker services.
REQUIREMENT = External dependency that must be installed.
RELEASE_BINARY = The built `target/release/infra` binary used by the smoke.
SSH_FIXTURE = The sshd-capable Docker fixture container used for the SSH phase.

[CONTENT]
# Integration quickstart

Infra ships a high-signal [INTEGRATION_SMOKE] that stands up local services and
verifies critical flows (Postgres, SSH, HTTP).

## Requirements

- Docker ([REQUIREMENT])
- Docker daemon access for the current user (for example, local socket permission)

## Run

From repo root:

```bash
./tools/smoke
```

What it does:
- Builds [RELEASE_BINARY] before running the MCP handshake.
- Starts [SSH_FIXTURE] without per-run package installation.
- Starts ephemeral Postgres + SSH containers.
- Starts a local HTTP server for pipeline tests.
- Runs a focused end-to-end validation and exits fail-closed on errors.

If the smoke test fails, review logs and re-run after fixes.
