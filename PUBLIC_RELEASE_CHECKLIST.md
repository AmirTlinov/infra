[LEGEND]
RELEASE = A public artifact or tag intended for external users.
CHECKLIST = A minimal, deterministic set of pre-release checks.

[CONTENT]
# Public release checklist

Use this [CHECKLIST] before any [RELEASE].

## Quality gates

- Run `./tools/doctor` ([DOCTOR]) and fix any diagnostics.
- Run `./tools/gate` ([GATE]) and ensure it passes.
- Verify smoke: `./tools/smoke` (if Docker is available).

## Security and safety

- Ensure no secrets or keys are committed.
- Confirm `INFRA_UNSAFE_LOCAL` defaults to disabled.
- Review any changes to profile storage or redaction behavior.

## Docs and contracts

- Update docs and contracts first when behavior changes.
- Ensure new tools/actions are reflected in the tool catalog.
- If behavior changed, add/refresh regression tests.

## Packaging

- Verify release binary builds cleanly.
- Record version bump and release notes if applicable.
