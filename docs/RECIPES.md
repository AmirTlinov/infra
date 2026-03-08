 [LEGEND]
RECIPE = A copy/paste example: request → expected output shape.
EFFECTS = Side-effect metadata for an action: `{ kind, requires_apply, irreversible }`.
APPLY = The `apply=true` opt-in flag required when `EFFECTS.requires_apply=true`.
CONFIRM = The `confirm=true` explicit acknowledgement required when `EFFECTS.irreversible=true`.
DRY_RUN = Compile + preview only (no execution).
ARTIFACT = Stored output captured by a tool (diff/export/logs) and returned as a reference.

 [CONTENT]
# Recipes (intent → expected artifact)

These are **high-signal** workflows intended for AI agents and humans.

Each block below is a [RECIPE]. In particular:
- Workflow tools attach [EFFECTS] (read/write/irreversible).
- Many flows produce a stored [ARTIFACT] reference (diff/export/logs) you can pass to the next step.

## 1) Discover “what can I do here?”

RECIPE:
```json
{ "action": "diagnose", "format": "actions", "include_call": true, "limit": 25 }
```

Expected:
- `actions[]` contains copy/paste tool calls, each with `effects` (read/write/irreversible).
- Warnings/hints tell you what is missing (profiles, store location, bindings).

## 2) Deploy (safe preview)

RECIPE ([DRY_RUN]):
```json
{
  "action": "run",
  "intent_type": "deploy",
  "apply": false,
  "inputs": { "overlay": "./overlays/prod" }
}
```

Expected:
- `dry_run=true`
- `plan.effects.kind="write"` and `plan.effects.requires_apply=true`
- `preview[]` shows the selected runbook + resolved inputs.

## 3) Deploy (apply)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "deploy",
  "apply": true,
  "inputs": { "overlay": "./overlays/prod" }
}
```

Expected:
- Runbook step outputs for `kubectl kustomize` + `kubectl apply` (stdout/stderr + exit codes).

## 4) Rollback (kubectl rollout undo)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "rollback",
  "apply": true,
  "inputs": {
    "namespace": "default",
    "workload_kind": "deploy",
    "workload_name": "api",
    "wait": true
  }
}
```

Expected:
- Undo output + (optional) rollout status output.

## 5) DB migration (Postgres, transactional)

RECIPE (requires [APPLY] + [CONFIRM]):
```json
{
  "action": "run",
  "intent_type": "db.migrate",
  "apply": true,
  "confirm": true,
  "inputs": {
    "statements": [
      { "sql": "CREATE TABLE IF NOT EXISTS example(id bigserial primary key)" },
      { "sql": "INSERT INTO example(id) VALUES (1) ON CONFLICT DO NOTHING" }
    ]
  }
}
```

Expected:
- `results[]` per statement (rows/rowCount/duration_ms etc).
- If you omit `confirm=true` you should receive a **deny** error (irreversible effects).

## 6) Incident triage (Kubernetes)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "incident.triage",
  "inputs": {
    "namespace": "default",
    "selector": "app=api",
    "workload_kind": "deploy",
    "workload_name": "api",
    "logs": true,
    "tail_lines": 200
  }
}
```

Expected:
- Cluster snapshot (events/pods/describe/logs) in step outputs.

## 7) Repo triage (git status + diff)

RECIPE:
```json
{ "action": "run", "intent_type": "repo.triage", "inputs": { "repo_root": "/repo" } }
```

Expected:
- `git status` (porcelain) + last commit + `git diff` snapshot in step outputs.

## 8) First run (bootstrap preflight)

RECIPE:
```json
{ "action": "run", "intent_type": "preflight.bootstrap" }
```

Expected:
- Workspace diagnostics + best-effort checks for docker/kubectl/ssh (with clear errors if missing).
- SSH/API checks run with `stability:"auto"` by default in flagship runbooks.

## 9) E2E smoke (target baseline)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "smoke.baseline",
  "inputs": { "project": "venorus", "target": "prod", "api_url": "/health", "sql": "SELECT 1" }
}
```

Expected:
- `plan.effects.kind="read"` (no [APPLY]/[CONFIRM] needed).
- Step outputs include: SSH check + DB ping + HTTP health check.
- SSH/API checks use `stability:"auto"` to tolerate transient channel flaps.

## 10) Smoke (HTTP endpoint)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "smoke.http",
  "inputs": { "url": "https://example.com/health", "expect_code": 200 }
}
```

Expected:
- Status code assertion + short response summary (and body capture if enabled in tool config).
- Retry noise stays compact unless a transient flap actually happened.

## 11) Smoke (render manifests → images summary)

RECIPE:
```json
{
  "action": "run",
  "intent_type": "smoke.manifests.images",
  "inputs": { "repo_root": "/repo", "path": "./manifests" }
}
```

Expected:
- Plain render output (images list / counts) you can diff or feed into deploy checks.
