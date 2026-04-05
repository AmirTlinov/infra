[LEGEND]
CLI_ROUTER = Thin command router that normalizes action payloads, emits one JSON envelope, and sets exit codes from real state.
DESCRIPTION_SNAPSHOT = One combined view of capabilities + runbooks with shared hash, versions, sources, and load time.
CANONICAL_SURFACES = The public CLI groups: `describe`, `target`, `profile`, `capability`, `policy`, `operation`, `receipt`, `job`.
INTENT_KERNEL = Capability -> runbook -> tool execution path used by `operation`.
RECEIPT_BUNDLE = One persisted result package containing operation outcome, verification, evidence summary, step trace, job outcomes, artifacts, and description snapshot.
LIVE_STATUS = Status derived from persisted operation state plus current background-job state, not from a stale stored string.
BACKGROUND_JOB = Provider-neutral stored job record whose live state is folded back into operation/receipt views.
TOOLING_LAYER = Canonical tool naming, contracts, and effect rules shared by managers and execution.

[CONTENT]
Infra is organized around a CLI-first operator path, not around a hidden transport session.

Execution path:
- [ENTRYPOINT|LEGEND.md]: `src/main.rs` runs `infra::cli::run()`.
- [CLI_ROUTER]: `src/cli.rs` parses `infra <surface> <action>`, merges `--json` / `--json-file` / `--arg`, calls one manager, and emits one JSON envelope.
- [APP_WIRING|LEGEND.md]: `src/app.rs` constructs services and managers once per CLI invocation.
- [TOOLING_LAYER]: `src/tooling/` carries canonical tool names, contract catalog lookup, and effect resolution.

Canonical public surfaces ([CANONICAL_SURFACES]):
- `describe status`
- `target list|get|resolve`
- `profile list|get|set|delete`
- `capability list|get|resolve|families`
- `policy resolve|check`
- `operation observe|plan|apply|verify|rollback|status|cancel|list`
- `receipt list|get`
- `job status|wait|logs|cancel|list`

Description and context lifecycle:
- [DESCRIPTION_SNAPSHOT] lives in `src/services/description.rs`.
- Each CLI invocation recomputes one combined snapshot for capabilities + runbooks.
- `ContextService` no longer keeps a session cache in the hot path, so filesystem-derived context is recomputed instead of silently reused.
- Operation receipts persist the [DESCRIPTION_SNAPSHOT] they were executed with.

Operation kernel:
- `src/managers/operation.rs` is the main [INTENT_KERNEL] surface.
- `observe` and `verify` are no longer the same contract:
  - `observe` reads state;
  - `verify` requires explicit checks and returns a strict verdict.
- `rollback` is tied to `from_operation_id` and refuses synthetic rollback without a real write trace.

State truth:
- `src/utils/operation_view.rs` computes [LIVE_STATUS] from the persisted receipt plus current [BACKGROUND_JOB] state.
- An operation cannot remain `completed` while a linked job is still `running` or `waiting_external`.
- `src/managers/receipt.rs` exposes a [RECEIPT_BUNDLE] instead of a thin pointer to separate evidence.

Target/profile/policy truth:
- `src/managers/target.rs` resolves expanded target bindings: profiles, paths, addresses, policy, and field provenance.
- `src/managers/profile.rs` is the canonical profile mutation surface.
- `src/managers/policy.rs` resolves policy and checks it explicitly instead of implying success from a plain read.
