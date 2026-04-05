[LEGEND]
RUNBOOK_GOAL = Turn repeated operator work into one declared, reusable path instead of replaying the same shell choreography every session.
GOOD_TRIGGER = A signal that the agent should stop repeating and package the flow.
BAD_TRIGGER = A case where making a runbook now would freeze confusion instead of reducing it.
SHAPE = The minimal assembly order for a useful runbook lane.
MANIFEST_HOME = The file-backed manifests where normal-mode runbooks and capabilities live.
SMALL_PATTERN = A small real pattern from this repo: one capability routes to one focused runbook with explicit inputs and effects.

[CONTENT]
Goal: [RUNBOOK_GOAL]

Use this file only when the active question is: “should I package this into a runbook?” or “how do I build one without creating a giant mess?”

[GOOD_TRIGGER]:
- the same prod/VPS/server task has already repeated across sessions;
- the manual flow has 3+ stable steps and the order matters;
- the work is safety-sensitive and should always carry the same `apply` / `confirm` / verify behavior;
- the action is a good operator primitive: smoke check, deploy, rollback, db change, triage snapshot, repo snapshot, preflight, sync, verify.

[BAD_TRIGGER]:
- you are still exploring and do not yet know the stable intent;
- every run needs a different improvisation;
- the proposed runbook is “incident universe”, “all deploy ops”, or another kitchen-sink bundle;
- the work is a one-off note or temporary debugging stunt that is unlikely to repeat.

[SHAPE]:
1. Reproduce the task manually once and identify the stable job name.
2. Shrink it to one operator promise, not a whole saga.
3. Define explicit inputs first: only what changes between runs.
4. Build one focused runbook in [MANIFEST_HOME] with small steps that reuse existing tool families (`api`, `ssh`, `sql`, `repo`, `local`, `pipeline`, `vault`, etc.).
5. Add or update one capability that points to that runbook, declares the intent, tags, input defaults/maps, and effects.
6. Keep read/preflight, write/apply, and verify semantics honest. If it writes, require `apply`; if it is irreversible, require `confirm`.
7. Validate the new lane through `infra describe status`, the relevant operation/receipt path, and repo proof rails.

[MANIFEST_HOME]:
- `runbooks.json`
- `capabilities.json`

Normal-mode rule:
- edit the manifests; do not rely on runtime-only mutable runbook APIs.

[SMALL_PATTERN]:
Capability:
```json
"smoke.http.endpoint": {
  "intent": "smoke.http",
  "description": "HTTP smoke: fetch URL and assert expected status code.",
  "runbook": "smoke.http.endpoint",
  "tags": ["smoke", "http", "read"],
  "inputs": {
    "required": ["url"],
    "defaults": { "expect_code": 200 },
    "map": {}
  },
  "effects": { "kind": "read", "requires_apply": false }
}
```

Runbook:
```json
"smoke.http.endpoint": {
  "description": "HTTP smoke: fetch URL and assert expected status code.",
  "inputs": ["url", "expect_code"],
  "steps": [
    {
      "id": "http",
      "tool": "api",
      "args": {
        "action": "smoke_http",
        "url": "{{ input.url }}",
        "expect_code": "{{ input.expect_code }}",
        "follow_redirects": true,
        "insecure_ok": true,
        "stability": "auto"
      }
    }
  ]
}
```

What this pattern teaches:
- one clear intent;
- one focused runbook;
- explicit changing inputs;
- defaults only where safe;
- effects declared at the capability layer;
- no giant workflow for unrelated jobs.

Good naming rule:
- `<domain>.<job>` or `<domain>.<subdomain>.<job>`
- prefer names like `smoke.http.endpoint`, `repo.snapshot`, `k8s.rollback.undo`
- avoid vague names like `prod.fix`, `server.ops`, `misc.runbook`

Fast decision rule:
- after the second honest repetition, or the first painful 3+ step prod loop, stop and package;
- if the procedure is still fuzzy, do one more learning pass first, then package the smallest stable slice.
