[LEGEND]
FLAT = Short scalar input passed as `--arg key=value`.
NESTED = Structured input passed as JSON.
CHECKS = Explicit verification conditions for `operation verify`.
FULL_CONTEXT = The explicit routing inputs that reduce false matches and false environment inference.

[CONTENT]
Payload rules:
- Use [FLAT] `--arg key=value` for short scalar inputs.
- Use [NESTED] `--json '{...}'` or `--json-file <path>` for arrays, objects, or large payloads.
- Prefer [FULL_CONTEXT] whenever it exists: `project`, `target`, `repo_root`, `cwd`, `family`, `intent`, `capability`, `operation_id`.

Do not rely on ambient repo guesses when you can pass explicit context.

Verification rule:
- `infra operation verify` is only meaningful with explicit [CHECKS].
- Keep checks small and concrete; verify the condition that actually proves success.

Examples:
```bash
infra target resolve \
  --arg project=demo \
  --arg name=prod

infra capability resolve \
  --arg intent=deploy.observe \
  --arg project=demo \
  --arg target=prod \
  --arg repo_root=/srv/demo

infra operation verify \
  --arg family=deploy \
  --arg project=demo \
  --arg target=prod \
  --arg 'checks=[{"path":"results.0.result.success","equals":true}]'

infra operation apply \
  --json '{
    "family":"deploy",
    "project":"demo",
    "target":"prod",
    "input":{"release":"2026.04.05"}
  }'
```

When choosing between `--arg` and `--json`:
- few scalar keys: `--arg`
- arrays, nested objects, long strings, or many fields: `--json` or `--json-file`

If you are debugging a surprising route:
1. rerun with explicit `project + target + repo_root`
2. rerun `infra describe status`
3. only then inspect deeper details
