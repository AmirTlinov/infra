[LEGEND]
OBSERVE = Read current state without turning it into a verdict.
VERIFY = Read plus explicit checks plus strict verdict.
TRACE = Recorded write history that allows status, receipt, and rollback to refer to the same real action.
BUNDLE = Unified receipt payload with result summary, evidence summary, logs, artifacts, and job outcomes.

[CONTENT]
Do not collapse semantics.

Core distinctions:
- [OBSERVE] answers "what is the current state?"
- [VERIFY] answers "did the required condition pass?"
- `apply` answers "perform the action"
- `rollback` answers "build a compensating action from a real [TRACE]"

Strict rules:
- `verify` without explicit checks is not real verification.
- `rollback` without a real prior operation trace is not real rollback.
- `state=waiting_external` means the operation is not done yet.
- A timeout inside one internal step is not proof of success.

Long-running work:
- Use `infra operation status` for the live operation view.
- Use `infra job status|wait|logs|cancel` when the operation links to background work.
- Do not say "done" until the linked jobs are terminal and the receipt bundle contains the proof you need.

Receipt semantics:
- `infra receipt get` should be the first place you look for closure.
- Treat the receipt as the [BUNDLE]: summary, verification/evidence summary, logs, artifacts, and job outcomes together.
- If the receipt is missing the proof you need, report that gap directly instead of narrating success.

Small closure loop:
```bash
infra operation apply ...
infra operation status --arg operation_id=<operation-id>
infra job wait --arg operation_id=<operation-id>
infra receipt get --arg operation_id=<operation-id>
infra operation verify --arg operation_id=<operation-id> --arg 'checks=[...]'
```
