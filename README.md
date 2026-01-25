# Infra

Infra — production‑grade stdio MCP‑сервер для операционных задач AI‑агентов: SSH, HTTP‑клиент, Postgres, runbooks, pipelines, intents, artifacts, audit и state.

## Для кого
- Команды, которым нужны безопасные и воспроизводимые ops‑действия через MCP.
- AI‑агенты, которым нужен единый, быстрый и детерминированный интерфейс к инфраструктуре.

## Что внутри
- **Ops‑инструменты**: SSH/API/SQL/Repo/Pipelines.
- **Runbooks/Intents**: оркестрация многошаговых действий.
- **Audit/Evidence**: следы выполнения и артефакты для отладки.
- **DX**: строгие схемы, алиасы действий, фильтры списка, читаемые ошибки.

## Быстрый старт (локально)
1) Диагностика:

`./tools/doctor`

2) Гейты качества (fmt + clippy + tests):

`./tools/gate`

3) Запуск MCP‑сервера:

`cargo run --release`

## Рекомендуемая конфигурация (без смешения проектов)
Чтобы агент не видел runbooks других проектов, изолируй профили:
- `MCP_PROFILES_DIR=/path/to/project/.infra`

Точечные пути при необходимости:
- `MCP_RUNBOOKS_PATH=/path/to/project/.infra/runbooks.json`
- `MCP_CAPABILITIES_PATH=/path/to/project/.infra/capabilities.json`
- `MCP_CONTEXT_REPO_ROOT=/path/to/project/.infra/artifacts`

## Примеры запросов
Список runbooks:
```
{"action":"list","query":"k8s","tags":["gitops"],"limit":20}
```

Запуск runbook:
```
{"action":"run","name":"k8s.diff","input":{"overlay":"./overlays/dev"}}
```

## Документация
- `mcp_config.md` — конфиг MCP‑клиента
- `docs/INTEGRATION.md` — интеграционные проверки
- `docs/RUNBOOK.md` — гайды по runbooks
- `SECURITY.md` — безопасность
- `PUBLIC_RELEASE_CHECKLIST.md` — релиз‑гигиена

## Ключевые файлы
- `src/main.rs` — stdio entrypoint
- `src/mcp/server.rs` — MCP routing
- `src/app.rs` — wiring (DI)
