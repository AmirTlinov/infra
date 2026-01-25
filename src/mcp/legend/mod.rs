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
            "shape": "По умолчанию инструменты возвращают строгий JSON envelope (для парсинга). Параллельно пишется .context артефакт для человека и `result.json` для машины (если настроен context repo root).",
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
            "preset": {
                "meaning": "Применить сохранённый preset до мерджа аргументов. Синонимы: `preset` и `preset_name`.",
                "merge_order": [
                    "1) preset.data (по имени)",
                    "2) alias.args (если вызвали алиас)",
                    "3) arguments вызова (побеждают)",
                ],
            },
            "tracing": {
                "meaning": "Корреляция вызовов для логов/аудита/трасс. Можно прокидывать сверху вниз.",
                "fields": ["`trace_id`", "`span_id`", "`parent_span_id`"],
            },
            "response_mode": {
                "meaning": "Формат ответа на этот tool-call: `ai|compact` (строгий JSON).",
                "values": ["`ai`", "`compact`"],
                "note": "`compact` сейчас эквивалентен `ai` (зарезервировано на будущее). Сервер пишет `result.json` (JSON-артефакт) и возвращает `artifact_uri_json`, если настроен context repo root.",
            },
        },
        "resolution": {
            "tool_aliases": Value::Object(aliases),
            "tool_resolution_order": [
                "Точное имя инструмента (например, `mcp_ssh_manager`).",
                "Встроенные алиасы (`ssh`, `psql`, `api`, …).",
                "Пользовательские алиасы из `mcp_alias` (могут добавлять args/preset).",
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
            "intent_apply": {
                "meaning": "Intent с write/mixed effects требует `apply: true` (иначе будет ошибка).",
            },
            "unsafe_local": {
                "meaning": "`mcp_local` доступен только при включённом unsafe режиме; в обычном режиме он скрыт из `tools/list`.",
                "gate": "`INFRA_UNSAFE_LOCAL=1`",
            },
        },
        "golden_path": [
            "1) `help()` → увидеть инструменты.",
            "2) `legend()` → понять семантику общих полей и resolution.",
            "3) (опционально) `mcp_project.project_upsert` + `mcp_project.project_use` → связать project/target с профилями.",
            "4) Дальше работать через `ssh`/`env`/`psql`/`api` с `target` и минимальными аргументами.",
        ],
    })
}
