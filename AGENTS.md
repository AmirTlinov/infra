# Infra — Agent Rules (Golden Path)

This repo is optimized for humans + AI agents to collaborate without guesswork.

Golden path:
- Run `./tools/doctor` first (diagnose).
- Then run `./tools/gate` (fail-closed correctness gate).

Doc standard:
- Repo-root `AGENTS.md` and repo-root `README.md` are freeform Markdown.
- Every other `.md` doc MUST be written in the context format:
  - A `[LEGEND]` block (definitions)
  - A `[CONTENT]` block (uses definitions)

Change protocol (contracts-first):
1) Update contracts/interfaces first.
2) Update implementation.
3) Update tests.
4) Update docs (context format).

Boundaries:
- Prefer explicit contracts over “conventions”.
- Prefer deterministic checks over “tribal knowledge”.
