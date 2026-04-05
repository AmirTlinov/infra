[LEGEND]
BOOTSTRAP = Repair path when `infra` is missing or not usable from a normal shell.
VERIFY = Minimal proof that the global binary and the active descriptions are visible.
GLOBAL_SKILL = Skill package under `~/.codex/skills/infra/` so future sessions can discover it outside this repo.

[CONTENT]
Only open this file when `infra` is missing from PATH, points to the wrong binary, or the [GLOBAL_SKILL] is missing.

[BOOTSTRAP]:
1. Build or install the binary:
```bash
cargo install --path /absolute/path/to/infra --root ~/.local --force
```
2. Confirm the binary is on PATH:
```bash
command -v infra
infra --help
```
3. Confirm it works outside the repo:
```bash
cd /tmp
infra describe status
```
4. If the skill itself is missing globally, install or sync the skill package to:
```bash
~/.codex/skills/infra/
```

[VERIFY]:
- `command -v infra` returns a path on PATH
- `infra describe status` returns `description_snapshot`
- the same command works from a different working directory

Do not trust the setup until those checks pass.
