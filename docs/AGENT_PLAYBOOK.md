 [LEGEND]
GOLDEN_PATH = The recommended sequence that keeps work safe and deterministic.
ADD_TOOL = The safe workflow for adding a new tool handler.

 [CONTENT]
Golden path ([GOLDEN_PATH]):
1) Read MAP.md.
2) If you are changing an interface, follow [CHANGE_PROTOCOL] (contracts → implementation → tests → docs).
3) Implement the change.
4) Add/adjust tests (prefer focused regression for the changed behavior).
5) Run `./tools/gate` until green (fail-closed).
6) Refresh the machine index: `./tools/context`.

Adding a tool ([ADD_TOOL]):
1) Follow `REPO_RULES.md` for module placement and wiring.
2) Add the manager under `src/managers/` and implement `ToolHandler`.
3) Wire it in `src/app.rs` (handlers + alias_map if needed).
4) Add a small regression test and run the [GATE].
