---
name: infra
description: Use the `infra` CLI as the direct prod-oriented operator surface. Start with `infra describe status`, stay on the CLI path, and open only one deeper reference when you need command selection, payload syntax, result semantics, bootstrap repair, or repo-specific validation.
---

[LEGEND]
GOAL = Keep the agent on one direct operator path instead of rebuilding transport, cache, or session logic in chat.
ENTRY_CHECK = The first command that proves which descriptions are active right now.
DEFAULT_LOOP = The smallest canonical order through resolve, policy, operation, receipt, and job surfaces.
REF_COMMANDS = `references/commands.md`
REF_PAYLOADS = `references/payloads.md`
REF_SEMANTICS = `references/semantics.md`
REF_BOOTSTRAP = `references/bootstrap.md`
REF_VALIDATION = `references/repo-validation.md`
STOP_RULE = Conditions where the agent must stop guessing, stop bypassing the CLI, or stop claiming completion.

[CONTENT]
Goal: [GOAL]

[ENTRY_CHECK]:
```bash
infra describe status
```

Run this before trusting any other answer. It exposes the active description hash, sources, and load time.

[DEFAULT_LOOP]:
```bash
infra target resolve ...
infra profile get ...
infra policy check ...
infra capability resolve ...
infra operation observe ...
infra operation plan ...
infra operation apply ...
infra operation verify ...
infra receipt get ...
infra job status ...
infra job wait ...
```

Use only the shortest prefix you need. Do not expand into the whole loop unless the current blocker requires it.

Open only one deeper reference:
- Need to choose the next command or understand which surface owns what: [REF_COMMANDS]
- Need to pass arrays, nested JSON, explicit checks, or full context cleanly: [REF_PAYLOADS]
- Need to reason about `observe` vs `verify`, `waiting_external`, receipts, jobs, or rollback: [REF_SEMANTICS]
- `infra` is missing from PATH or the binary/skill needs bootstrap repair: [REF_BOOTSTRAP]
- You are changing this repo and need repo-owned proof rails: [REF_VALIDATION]

[STOP_RULE]:
- If `capability resolve` is ambiguous, stop and disambiguate. Do not pick one.
- If `verify` has no explicit checks, stop and add checks.
- If `state=waiting_external`, the work is still running.
- If the receipt bundle lacks the proof you need, say that directly.
- Do not bypass `infra` with ad-hoc provider or shell commands unless the user explicitly asks for a manual bypass.
