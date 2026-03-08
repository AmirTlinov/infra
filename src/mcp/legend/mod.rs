use crate::mcp::aliases::builtin_tool_aliases;
use crate::mcp::catalog::tool_by_name;
use crate::utils::feature_flags::is_unsafe_local_enabled;
use serde_json::Value;

fn builtin_aliases_for_legend() -> serde_json::Map<String, Value> {
    let unsafe_local = is_unsafe_local_enabled();
    let mut out = serde_json::Map::new();
    for (alias, tool_name) in builtin_tool_aliases().iter() {
        if !unsafe_local && *tool_name == "mcp_local" {
            continue;
        }
        if tool_by_name(tool_name).is_none() {
            continue;
        }
        out.insert(
            (*alias).to_string(),
            Value::String((*tool_name).to_string()),
        );
    }
    out
}

pub fn build_legend_payload() -> Value {
    let aliases = builtin_aliases_for_legend();
    serde_json::json!({
        "name": "legend",
        "description": "Каноничная семантика Infra: общие поля, порядок разрешения и безопасные дефолты.",
        "mental_model": [
            "Думайте об Infra как о «наборе адаптеров + память»: вы вызываете tool+action и получаете результат (который можно дополнительно сформировать через `output` и/или сохранить через `store_as`).",
            "Основная UX-ось: один раз связать `project`+`target` с профилями → дальше вызывать `ssh`/`env`/`psql`/`api` только с `target`.",
        ],
        "response": {
            "shape": "По умолчанию инструменты возвращают строгий JSON envelope (для парсинга) и дублируют его в MCP `structuredContent`. Параллельно пишется .context артефакт для человека и `result.json` для машины (если настроен context repo root).",
            "tracing": "Корреляция (`trace_id`/`span_id`/`parent_span_id`) пишется в audit log и логи (stderr). Для просмотра используйте `mcp_audit`.",
        },
        "common_fields": {
            "action": {
                "meaning": "Операция внутри инструмента. Почти всегда обязательна (см. `help({tool})` чтобы увидеть enum).",
                "example": { "tool": "mcp_ssh_manager", "action": "exec" },
            },
            "output": {
                "meaning": "Формирует возвращаемое значение (и то, что попадёт в `store_as`).",
                "pipeline": "`path` → `pick` → `omit` → `map`",
                "path_syntax": [
                    "Dot/bracket: `rows[0].id`, `entries[0].trace_id`",
                    "Числа в `[]` считаются индексами массива.",
                ],
                "missing": {
                    "default": "`error` (бросает ошибку)",
                    "modes": [
                        "`error` → ошибка, если `path` не найден или `map` ожидает массив",
                        "`null` → вернуть `null`",
                        "`undefined` → вернуть `undefined`",
                        "`empty` → вернуть «пустое значение» (обычно `{}`; если используется `map` — `[]`)",
                    ],
                },
                "default": {
                    "meaning": "Если `missing` не `error`, можно задать явный `default` (он также участвует в `map`).",
                },
            },
            "store_as": {
                "meaning": "Сохранить сформированный результат в `mcp_state`.",
                "forms": [
                    "`store_as: \"key\"` + (опционально) `store_scope: \"session\"|\"persistent\"`",
                    "`store_as: { key: \"key\", scope: \"session\"|\"persistent\" }`",
                ],
                "note": "`session` — дефолт, если scope не указан.",
            },
            "apply": {
                "meaning": "Явный opt-in для write/mixed effects: если действие помечено как `write`/`mixed` (см. `meta.effects.requires_apply=true`), без `apply: true` сервер вернёт deny.",
                "example": { "tool": "mcp_repo", "action": "apply_patch", "apply": true, "repo_root": "/repo", "patch": "<diff>" },
            },
            "confirm": {
                "meaning": "Явное подтверждение необратимости: если действие помечено как `irreversible` (см. `meta.effects.irreversible=true`), без `confirm: true` сервер вернёт deny.",
                "example": { "tool": "mcp_env", "action": "profile_delete", "confirm": true, "profile_name": "prod-env" },
            },
            "preset": {
                "meaning": "`preset` / `preset_name` теперь compatibility-only: generic runtime hot path их отклоняет с migration hint.",
                "merge_order": [
                    "1) используйте явные arguments вызова",
                    "2) стабильные defaults переносите в project/target/profile config",
                    "3) `mcp_preset` оставляйте только как legacy storage/list surface",
                ],
            },
            "tracing": {
                "meaning": "Корреляция вызовов для логов/аудита/трасс. Можно прокидывать сверху вниз.",
                "fields": ["`trace_id`", "`span_id`", "`parent_span_id`"],
            },
            "stability": {
                "meaning": "Опциональная политика устойчивости канала (`off|auto|aggressive` или объект overrides). Для `api` поле `stability` имеет приоритет над legacy `retry`.",
                "overrides": [
                    "`stability=off` — без ретраев/circuit-breaker.",
                    "`stability=auto` — сбалансированные bounded retry + circuit-breaker.",
                    "`stability=aggressive` — больше попыток и окно circuit-breaker.",
                ],
                "quiet_default": "Поле `result.stability` возвращается только если был retry/деградация (или включён debug).",
            },
            "response_mode": {
                "meaning": "Формат ответа на этот tool-call: `ai|compact` (строгий JSON).",
                "values": ["`ai`", "`compact`"],
                "note": "`compact` сейчас эквивалентен `ai` (зарезервировано на будущее). Сервер пишет `result.json` (JSON-артефакт) и возвращает `artifact_uri_json`, если настроен context repo root.",
            },
        },
        "resolution": {
            "tool_aliases": Value::Object(aliases),
            "tools_list": "В `tools/list` по умолчанию публикуется low-entropy core tier (`INFRA_TOOL_TIER=core`); `INFRA_TOOL_TIER=expert` включает расширенную каноническую поверхность. Встроенные короткие алиасы остаются совместимыми при явном `tools/call`.",
            "tool_resolution_order": [
                "Точное имя инструмента (например, `mcp_ssh_manager`).",
                "Встроенные алиасы (`ssh`, `psql`, `api`, …).",
                "Пользовательские алиасы из `mcp_alias` (могут добавлять args; preset inheritance в runtime hot path отклоняется).",
            ],
            "project": {
                "meaning": "Именованный набор target-ов, каждый target связывает профили/пути/URL.",
                "resolved_from": ["`project` или `project_name` в аргументах", "active project из state (`project.active`)"],
            },
            "target": {
                "meaning": "Окружение внутри project (например, `prod`, `stage`).",
                "synonyms": ["`target`", "`project_target`", "`environment`"],
                "selection": [
                    "явно через аргументы (synonyms)",
                    "иначе `project.default_target`",
                    "иначе auto-pick если target ровно один",
                    "иначе ошибка (когда target-ов несколько)",
                ],
            },
            "profile_resolution": {
                "meaning": "Как выбирается `profile_name`, если вы его не указали.",
                "order": [
                    "если есть inline `connection` → он используется напрямую",
                    "иначе `profile_name` (явно)",
                    "иначе binding из `project.target.*_profile` (если project/target резолвятся)",
                    "иначе auto-pick если профиль ровно один в хранилище этого типа",
                    "иначе ошибка",
                ],
            },
        },
        "refs": {
            "env": {
                "scheme": "`ref:env:VAR_NAME`",
                "meaning": "Подставить значение из переменной окружения (для секретов/паролей/ключей).",
            },
            "vault": {
                "scheme": "`ref:vault:...` (например, `ref:vault:kv2:secret/app/prod#TOKEN`)",
                "meaning": "Подставить значение из HashiCorp Vault (KV v2). Требует выбранного `vault_profile`.",
            },
        },
        "safety": {
            "secret_export": {
                "meaning": "Даже если есть `include_secrets: true`, экспорт секретов из профилей включается только break-glass флагом окружения.",
                "gates": ["`INFRA_ALLOW_SECRET_EXPORT=1`"],
            },
            "effects_apply": {
                "meaning": "Любой action с write/mixed effects требует `apply: true` (иначе будет deny). `meta.effects` всегда возвращается в ответе.",
            },
            "effects_confirm": {
                "meaning": "Любой action с irreversible effects требует `confirm: true` (иначе будет deny).",
            },
            "unsafe_local": {
                "meaning": "`mcp_local` доступен только при включённом unsafe режиме; в обычном режиме он скрыт из `tools/list`.",
                "gate": "`INFRA_UNSAFE_LOCAL=1`",
            },
            "preset_runtime": {
                "meaning": "Preset merge removed from generic executor hot path; explicit `preset` / `preset_name` now returns a compatibility error with migration hint.",
            },
        },
        "golden_path": [
            "1) `help()` → увидеть инструменты.",
            "2) `legend()` → понять семантику общих полей и resolution.",
            "3) `mcp_capability` + resources/prompts → выбрать capability family.",
            "4) `mcp_operation` → observe/plan/apply/verify с receipt-driven ответом.",
            "5) `mcp_project` использовать только когда нужно связать target/project metadata; raw SSH/HTTP/SQL surface оставлять для expert/debug path.",
        ],
    })
}
