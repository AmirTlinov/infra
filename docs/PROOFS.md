[LEGEND]
PROOF = A reproducible verification command + expected result.
TS_REPO = The legacy TypeScript repo, used only for parity comparison.

[CONTENT]
## Proofs (2026-01-24)

- [PROOF]: `./tools/doctor` → OK.
- [PROOF]: `./tools/gate` → OK (fmt + clippy `-D warnings` + tests).
- [PROOF]: `./tools/parity` → OK (Rust↔TS deterministic `tools/list` + `tools/call` parity; default `--suite extended` runs in safe+unsafe modes).
- [PROOF]: `./tools/smoke` → OK (docker-backed e2e smoke: api/postgres/ssh/artifacts + small load loop).

## Notes

- `./tools/parity` expects [TS_REPO] at `--ts-path` (or `INFRA_TS_PATH`, or the default Amir path).
- `./tools/parity` runs servers with isolated temp state (`MCP_PROFILES_DIR`) and isolated temp context roots (`INFRA_CONTEXT_REPO_ROOT`) to avoid polluting local profiles/context dirs.
