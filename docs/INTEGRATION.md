[LEGEND]
INTEGRATION_SMOKE = An end-to-end sanity run using local Docker services.
REQUIREMENT = External dependency that must be installed.

[CONTENT]
# Integration quickstart

Infra ships a high-signal [INTEGRATION_SMOKE] that stands up local services and
verifies critical flows (Postgres, SSH, HTTP).

## Requirements

- Docker ([REQUIREMENT])

## Run

From repo root:

```bash
./tools/smoke
```

What it does:
- Starts ephemeral Postgres + SSH containers.
- Starts a local HTTP server for pipeline tests.
- Runs a focused end-to-end validation and exits fail-closed on errors.

If the smoke test fails, review logs and re-run after fixes.
