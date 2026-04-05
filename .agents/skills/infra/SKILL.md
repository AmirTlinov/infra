---
name: infra
description: Use the `infra` CLI for production, prod, VPS, server, SSH, deploy, rollback, smoke, incident, runbook, and infrastructure work when the task should go through declared targets, profiles, policy, capabilities, operations, receipts, and jobs instead of ad-hoc shell commands. Start with `infra describe status`, stay on the CLI path, and open only one deeper reference when needed.
---

[LEGEND]
GOAL = Keep the agent on one direct operator path instead of rebuilding transport, cache, or session logic in chat.
INFRA_ONE_LINER = What `infra` is in one sentence.
HIGH_SIGNAL_SCENARIOS = The main operator jobs an agent can solve well through `infra`.
FUNCTION_MAP = What `infra` can control directly and what it can execute through active descriptions.
ENTITY_GLOSSARY = The minimum vocabulary an agent needs before using the CLI.
BOUNDARY_RULES = What not to assume about `infra`.
QUICK_EXAMPLES = Short commands that show the operator shape immediately.
ENTRY_CHECK = The first command that proves which descriptions are active right now.
STARTUP_BRANCH = What to do when `infra` is missing, broken, or not `ready`.
RUNBOOK_BRANCH = When repeated manual work should become a runbook instead of another ad-hoc session.
DEFAULT_LOOP = The smallest canonical order through resolve, policy, operation, receipt, and job surfaces.
REF_COMMANDS = `references/commands.md`
REF_PAYLOADS = `references/payloads.md`
REF_SEMANTICS = `references/semantics.md`
REF_BOOTSTRAP = `references/bootstrap.md`
REF_RUNBOOKS = `references/runbooks.md`
REF_VALIDATION = `references/repo-validation.md`
STOP_RULE = Conditions where the agent must stop guessing, stop bypassing the CLI, or stop claiming completion.

[CONTENT]
Goal: [GOAL]

[INFRA_ONE_LINER]:
`infra` is a CLI-first operator for declared infrastructure work: it resolves targets and profiles, chooses declared capabilities, executes controlled operations, and returns honest receipts and job state instead of ad-hoc shell guesswork.

[HIGH_SIGNAL_SCENARIOS]:
- understand what descriptions are active right now and whether the agent is looking at the right prod picture;
- resolve the real binding for a target: profile, paths, kubeconfig, addresses, policy context, provenance;
- inspect whether a write is allowed before attempting it;
- resolve and run declared operator flows such as smoke, deploy, rollback, triage, db work, or repo work;
- verify the result with explicit checks instead of narrating success;
- follow background work honestly until it is really done and the receipt bundle contains proof.

[FUNCTION_MAP]:
- Direct CLI control surfaces:
  - `describe`
  - `target`
  - `profile`
  - `capability`
  - `policy`
  - `operation`
  - `receipt`
  - `job`
  - `runbook`
- Built-in execution families that active capabilities/runbooks may use under the hood:
  - HTTP/API checks and calls
  - SSH checks and commands
  - SQL/Postgres queries
  - repo/git work
  - local/workspace/fs/env work
  - pipeline/vault/state/context/artifact/evidence/audit helpers
- Typical declared flows often include things like smoke, deploy, rollback, db work, incident triage, or repo triage, but you must verify them through the active descriptions instead of assuming they exist.

[ENTITY_GLOSSARY]:
- `target` = the named environment or destination you want to operate on, after expansion into real bindings.
- `profile` = the concrete operator config and policy source that shapes how work is executed.
- `capability` = a declared action the descriptions say is allowed and how it should route.
- `operation` = one execution attempt with state, outputs, and possibly linked jobs.
- `receipt` = the canonical result bundle: summary, evidence summary, logs/artifacts, and job outcomes.
- `job` = long-running background work linked to an operation.

[BOUNDARY_RULES]:
- `infra` is not a generic replacement for `kubectl`, `terraform`, `aws`, `dig`, or raw bash.
- Do not assume Kubernetes, secrets, DNS, cloud resources, or rollout actions exist just because the repo has managers or examples for them.
- First inspect the active descriptions. If the loaded capabilities/runbooks do not expose the scenario, say that directly.
- Prefer `infra` over ad-hoc provider commands when the scenario is declared. Use manual bypass only when the user explicitly asks for it.

[QUICK_EXAMPLES]:
```bash
infra describe status
infra target resolve --arg project=demo --arg name=prod
infra operation verify --arg family=deploy --arg project=demo --arg target=prod --arg 'checks=[{"path":"results.0.result.success","equals":true}]'
```

[ENTRY_CHECK]:
```bash
infra describe status
```

Run this before trusting any other answer. It exposes the active description hash, sources, and load time.

[STARTUP_BRANCH]:
- If `infra` is missing from PATH, `infra describe status` fails, or the returned `state` is not `ready`, stop the normal loop and open [REF_BOOTSTRAP].
- Do not guess capabilities or start provider-specific manual work from a broken startup state.
- Resume the normal loop only after `infra describe status` succeeds and the state is `ready`.

[RUNBOOK_BRANCH]:
- If the same prod/server action is repeated across sessions, or the manual flow has three or more stable steps, or success depends on always doing the same read/write/verify sequence, stop repeating it and open [REF_RUNBOOKS].
- Do not create a runbook for one-off exploration, unclear diagnosis, or a still-moving procedure you do not understand yet.

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
- Need to package repeated work into a reusable runbook/capability pair: [REF_RUNBOOKS]
- You are changing this repo and need repo-owned proof rails: [REF_VALIDATION]

[STOP_RULE]:
- If `capability resolve` is ambiguous, stop and disambiguate. Do not pick one.
- If `verify` has no explicit checks, stop and add checks.
- If `state=waiting_external`, the work is still running.
- If the receipt bundle lacks the proof you need, say that directly.
- Do not bypass `infra` with ad-hoc provider or shell commands unless the user explicitly asks for a manual bypass.
