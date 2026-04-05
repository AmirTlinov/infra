[LEGEND]
READ = Read-only surface that should not mutate target state.
WRITE = Surface that can mutate target state or start a write-capable flow.
LONG = Surface for work that can outlive the immediate command.
BUNDLE = Unified result package with proof-oriented data.

[CONTENT]
Use the smallest surface that answers the active question.

Core map:
- `infra describe status` = [READ] active description hash, sources, and load time. Start here.
- `infra target resolve` = [READ] expanded target bindings with provenance for profile, paths, kubeconfig, addresses, and policy context.
- `infra profile get|list|set|delete` = canonical profile surface. Use `set` or `delete` only when the task is profile mutation.
- `infra policy resolve|check` = [READ] effective policy and whether a proposed action is allowed.
- `infra capability resolve` = [READ] strict intent-to-capability resolution on full context. If several candidates fit, it must fail loudly.
- `infra operation observe` = [READ] current state read.
- `infra operation plan` = [READ] preflight and intended action plan.
- `infra operation apply` = [WRITE] real execution path for a write or state-changing action.
- `infra operation verify` = [READ] strict checks and verdict, not just another read.
- `infra operation rollback` = [WRITE] compensating action built from a real prior operation trace.
- `infra operation status` = [READ] live state of an operation.
- `infra receipt get|list` = [BUNDLE] canonical result with summary, proof, logs, artifacts, and job outcomes.
- `infra job status|wait|logs|cancel` = [LONG] background work control surface.
- `infra runbook ...` = description/debug surface. Not the default operator path.

Default choice rule:
1. Need to know what is active right now: `describe status`
2. Need effective target facts: `target resolve`
3. Need to know whether a write is allowed: `policy check`
4. Need the right capability for an intent: `capability resolve`
5. Need to read, plan, write, verify, rollback, or watch operation state: `operation ...`
6. Need the proof/result bundle: `receipt get`
7. Need background tracking: `job ...`

Small examples:
```bash
infra target resolve --arg project=demo --arg name=prod
infra capability resolve --arg intent=deploy.observe --arg project=demo --arg target=prod
infra operation observe --arg family=deploy --arg project=demo --arg target=prod
infra receipt get --arg operation_id=<operation-id>
```
