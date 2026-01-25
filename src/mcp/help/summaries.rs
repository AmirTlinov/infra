use crate::utils::feature_flags::is_unsafe_local_enabled;

#[derive(Clone, Copy)]
pub struct Summary {
    pub description: &'static str,
    pub usage: &'static str,
}

pub fn primary_tool_alias(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "mcp_ssh_manager" => Some("ssh"),
        "mcp_psql_manager" => Some("psql"),
        "mcp_api_client" => Some("api"),
        "mcp_repo" => Some("repo"),
        "mcp_state" => Some("state"),
        "mcp_project" => Some("project"),
        "mcp_context" => Some("context"),
        "mcp_workspace" => Some("workspace"),
        "mcp_jobs" => Some("job"),
        "mcp_artifacts" => Some("artifacts"),
        "mcp_env" => Some("env"),
        "mcp_vault" => Some("vault"),
        "mcp_runbook" => Some("runbook"),
        "mcp_capability" => Some("capability"),
        "mcp_intent" => Some("intent"),
        "mcp_evidence" => Some("evidence"),
        "mcp_alias" => Some("alias"),
        "mcp_preset" => Some("preset"),
        "mcp_audit" => Some("audit"),
        "mcp_pipeline" => Some("pipeline"),
        "mcp_local" => Some("local"),
        _ => None,
    }
}

pub fn help_hint(tool_name: &str, action_name: Option<&str>) -> String {
    let tool = primary_tool_alias(tool_name).unwrap_or(tool_name);
    if let Some(action) = action_name {
        format!("help({{ tool: '{}', action: '{}' }})", tool, action)
    } else {
        format!("help({{ tool: '{}' }})", tool)
    }
}

pub fn is_core_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "help" | "legend" | "mcp_workspace" | "mcp_jobs" | "mcp_artifacts" | "mcp_project"
    )
}

pub fn summaries_ordered() -> Vec<(&'static str, Summary)> {
    let mut out = vec![
        (
            "help",
            Summary {
                description: "Показывает справку. Передайте `tool`, чтобы получить детали по инструменту.",
                usage: "call_tool → name: 'help', arguments: { tool?: string, action?: string }",
            },
        ),
        (
            "legend",
            Summary {
                description:
                    "Семантическая легенда: общие поля, порядок resolution, safety-гейты и golden path.",
                usage: "call_tool → name: 'legend' (или help({ tool: 'legend' }))",
            },
        ),
        (
            "mcp_psql_manager",
            Summary {
                description: "PostgreSQL: профили, запросы, транзакции, CRUD, select/count/exists/export + bulk insert.",
                usage: "profile_upsert/profile_list → query/batch/transaction → insert/insert_bulk/update/delete/select/count/exists/export",
            },
        ),
        (
            "mcp_ssh_manager",
            Summary {
                description: "SSH: профили, exec/batch, диагностика и SFTP.",
                usage: "profile_upsert/profile_list → (optional) authorized_keys_add → exec/exec_detached/exec_follow → job_* (tail_job/follow_job) → sftp_* (deploy_file)",
            },
        ),
        (
            "mcp_api_client",
            Summary {
                description: "HTTP: профили, request/paginate/download, retry/backoff, auth providers + cache.",
                usage: "profile_upsert/profile_list → request/paginate/download/check → smoke_http",
            },
        ),
        (
            "mcp_repo",
            Summary {
                description:
                    "Repo: безопасные git/render/diff/patch операции в sandbox + allowlisted exec без shell.",
                usage: "repo_info/git_diff/render → (apply=true) apply_patch/git_commit/git_revert/git_push → exec",
            },
        ),
        (
            "mcp_state",
            Summary {
                description: "State: переменные между вызовами, поддержка session/persistent.",
                usage: "set/get/list/unset/clear/dump",
            },
        ),
        (
            "mcp_project",
            Summary {
                description: "Projects: профили, targets и policy profiles для автономных сценариев.",
                usage: "project_upsert/project_list → project_use → (targets + policy_profiles)",
            },
        ),
        (
            "mcp_context",
            Summary {
                description: "Context: обнаружение сигналов проекта и сводка контекста.",
                usage: "summary/get → refresh → list/stats",
            },
        ),
        (
            "mcp_workspace",
            Summary {
                description: "Workspace: сводка, подсказки, диагностика.",
                usage: "summary/suggest → run → cleanup → diagnose → store_status",
            },
        ),
        (
            "mcp_jobs",
            Summary {
                description: "Jobs: единый реестр async задач (status/wait/logs/cancel/list).",
                usage: "job_status/job_wait/job_logs_tail/tail_job/follow_job/job_cancel/job_forget/job_list",
            },
        ),
        (
            "mcp_artifacts",
            Summary {
                description: "Artifacts: чтение и листинг artifact:// refs (bounded по умолчанию).",
                usage: "get/head/tail/list",
            },
        ),
        (
            "mcp_env",
            Summary {
                description: "Env: зашифрованные env-бандлы и безопасная запись/запуск на серверах по SSH.",
                usage: "profile_upsert/profile_list → write_remote/run_remote",
            },
        ),
        (
            "mcp_vault",
            Summary {
                description: "Vault: профили (addr/namespace + token или AppRole) и диагностика (KV v2).",
                usage: "profile_upsert/profile_list → profile_test",
            },
        ),
        (
            "mcp_runbook",
            Summary {
                description:
                    "Runbooks: хранение и выполнение многошаговых сценариев, плюс DSL.",
                usage: "runbook_upsert/runbook_upsert_dsl/runbook_list → runbook_run/runbook_run_dsl",
            },
        ),
        (
            "mcp_capability",
            Summary {
                description:
                    "Capabilities: реестр intent→runbook, граф зависимостей и статистика.",
                usage: "list/get/resolve → set/delete → graph/stats",
            },
        ),
        (
            "mcp_intent",
            Summary {
                description:
                    "Intent: компиляция и выполнение capability-планов с dry-run и evidence.",
                usage: "compile/explain → dry_run → execute (apply=true для write/mixed)",
            },
        ),
        (
            "mcp_evidence",
            Summary {
                description: "Evidence: просмотр сохранённых evidence-бандлов.",
                usage: "list/get",
            },
        ),
        (
            "mcp_alias",
            Summary {
                description: "Aliases: короткие имена для инструментов и аргументов.",
                usage: "alias_upsert/alias_list/alias_get/alias_delete",
            },
        ),
        (
            "mcp_preset",
            Summary {
                description: "Presets: reusable наборы аргументов для инструментов.",
                usage: "preset_upsert/preset_list/preset_get/preset_delete",
            },
        ),
        (
            "mcp_audit",
            Summary {
                description: "Audit log: просмотр и фильтрация событий.",
                usage: "audit_list/audit_tail/audit_stats/audit_clear",
            },
        ),
        (
            "mcp_pipeline",
            Summary {
                description: "Pipelines: потоковые HTTP↔SFTP↔PostgreSQL сценарии.",
                usage: "run/describe/deploy_smoke",
            },
        ),
    ];

    if is_unsafe_local_enabled() {
        out.push((
            "mcp_local",
            Summary {
                description:
                    "Local (UNSAFE): локальные exec и filesystem операции (только при включённом unsafe режиме).",
                usage: "exec/batch/fs_read/fs_write/fs_list/fs_stat/fs_mkdir/fs_rm",
            },
        ));
    }

    out
}
