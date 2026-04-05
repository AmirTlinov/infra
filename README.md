# Infra

Infra is a CLI-first operator binary for agents and humans who need a short, honest path to prod work.

The public path is now:

- `infra describe status`
- `infra target resolve`
- `infra profile get|set|delete`
- `infra capability resolve`
- `infra policy resolve|check`
- `infra operation observe|plan|apply|verify|rollback|status`
- `infra receipt get`
- `infra job status|wait|logs|cancel`

Every CLI call returns one JSON envelope with:

- `ok`
- `state`
- `summary`
- `description_snapshot`
- `result`
- `receipt` when an operation produced one

That means the agent no longer has to stitch together transport state, stale context, detached receipts, and separate evidence surfaces by hand.

## What changed

Infra no longer exposes a separate transport/server layer as the public operator path.

The canonical operator surface is the `infra` binary in `PATH`, and the repo-local `infra` skill is only a thin usage contract on top of that binary.

This removes the main source of drift:

- no separate long-lived description session,
- no stale context cache in the hot path,
- no silent capability choice under ambiguity,
- no fake success while background work is still running.

## Build

```bash
cargo build --release
```

Binary:

```bash
target/release/infra
```

## Runtime state and manifests

Infra still uses a project-local profile directory for manifests and mutable state:

```bash
mkdir -p .infra
export INFRA_PROFILES_DIR="$PWD/.infra"
```

Important:

- bundled `runbooks.json` and `capabilities.json` are available even outside a repo;
- project manifests still override bundled defaults;
- mutable operational state lives in SQLite (`infra.db`, override with `INFRA_STORE_DB_PATH`).

## Quick start

Inspect loaded descriptions:

```bash
infra describe status
```

Read a bundled capability:

```bash
infra capability get --arg name=repo.snapshot
```

Resolve an expanded target binding:

```bash
infra target resolve --arg project=demo --arg name=prod
```

Run a strict operation loop:

```bash
infra operation plan --arg family=deploy --arg project=demo --arg target=prod
infra operation apply --arg family=deploy --arg project=demo --arg target=prod --arg apply=true
infra operation verify --arg family=deploy --arg project=demo --arg target=prod --arg 'checks=[{"path":"results.0.result.success","equals":true}]'
```

Inspect the unified receipt bundle:

```bash
infra receipt get --arg operation_id=<operation-id>
```

Follow long work honestly:

```bash
infra job status --arg job_id=<job-id>
infra job wait --arg job_id=<job-id>
```

## Safe defaults

| Capability | Env var | Default |
|---|---|---|
| Local shell/filesystem access | `INFRA_UNSAFE_LOCAL=1` | off |
| Secret export | `INFRA_ALLOW_SECRET_EXPORT=1` | off |

## Validation

```bash
./tools/doctor
./tools/gate
```

## License

Apache-2.0
