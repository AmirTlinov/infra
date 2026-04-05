[LEGEND]
PROOF = A reproducible verification command + expected result.

[CONTENT]
## Proofs (2026-04-05)

- [PROOF]: `./tools/doctor` → OK.
- [PROOF]: `./tools/gate` → OK (fmt + clippy `-D warnings` + tests).
- [PROOF]: `./tools/smoke` → OK (docker-backed CLI smoke: target resolve + operation observe + unified receipt over api/postgres/ssh).
