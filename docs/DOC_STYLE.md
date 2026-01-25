[LEGEND]
CONTEXT_DOC = A Markdown doc written as `[LEGEND]` then `[CONTENT]`.

[CONTENT]
Doc format (mandatory for every [CONTEXT_DOC]):
- Repo-root `AGENTS.md` and repo-root `README.md` are freeform Markdown.
- Every other `.md` MUST be a context doc:
  - First non-empty line is `[LEGEND]`.
  - Then a `[CONTENT]` block.

Token rules:
- Define shared meanings in `LEGEND.md` as `TOKEN = Meaning`.
- Define doc-specific meanings in the doc’s own `[LEGEND]`.
- Do not shadow: a doc MUST NOT redefine a global token locally ([NO_SHADOWING]).
- Use tokens in content via `[TOKEN]` (optionally `[TOKEN|LEGEND.md]`).
