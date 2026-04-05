[LEGEND]
GOLDEN_PATH = Short sequence that keeps operator work honest and reproducible.
CANONICAL_LOOP = Preferred prod loop through `infra` CLI surfaces before any raw expert detour.
RAW_DETOUR = Direct expert-manager usage outside the canonical loop; allowed only when the CLI surface cannot answer the question.

[CONTENT]
Golden path ([GOLDEN_PATH]):
1) Read `MAP.md`.
2) If the change touches contracts, follow contracts -> implementation -> tests -> docs.
3) Run `./tools/doctor`.
4) Use the [CANONICAL_LOOP].
5) Run `./tools/gate`.

[CANONICAL_LOOP]:
1) `infra describe status`
2) `infra target resolve` / `infra profile get` / `infra policy check`
3) `infra capability resolve`
4) `infra operation observe|plan|apply|verify`
5) `infra receipt get`
6) `infra job status|wait` when background work exists

Rules:
- Prefer `--json` for complex payloads and `--arg key=value` for short calls.
- Treat `receipt` as the canonical result package; do not separately hunt for evidence unless the receipt itself says it is missing.
- Treat `waiting_external` as not done.
- Treat `verify` without explicit checks as invalid work, not as a soft read.
- Treat ambiguity as a stop signal; do not guess between capabilities.

[RAW_DETOUR]:
- Allowed only when the CLI surface truly cannot express the needed action.
- If you detour, say why the canonical CLI surface was insufficient.
