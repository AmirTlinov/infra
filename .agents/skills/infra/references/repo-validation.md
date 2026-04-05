[LEGEND]
VALIDATION_DOCTOR = Fast repo-owned environment sanity check.
VALIDATION_GATE = Fail-closed repo-owned correctness gate.
SMOKE = Installed-binary / CLI-path proof when the change touches discovery, packaging, PATH, or skill routing.

[CONTENT]
When you are changing this repo itself, validate with repo-owned rails:

[VALIDATION_DOCTOR]:
```bash
./tools/doctor
```

[VALIDATION_GATE]:
```bash
./tools/gate
```

Use [SMOKE] when the change affects CLI availability, PATH setup, discovery, or skill/bootstrap ergonomics:
```bash
./tools/smoke --load-n 10
```

Minimal order:
1. `./tools/doctor`
2. make the change
3. `./tools/gate`
4. run `./tools/smoke --load-n 10` if you changed install/path/skill/operator-entry behavior
