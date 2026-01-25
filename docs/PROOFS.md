 [LEGEND]
PROOF = A reproducible verification command + expected result.

[CONTENT]
## Proofs (2026-01-24)

- [PROOF]: `./tools/doctor` → OK.
- [PROOF]: `./tools/gate` → OK (fmt + clippy `-D warnings` + tests).
- [PROOF]: `./tools/smoke` → OK (docker-backed e2e smoke: api/postgres/ssh/artifacts + small load loop).
