use crate::mcp::aliases::canonical_tool_name;
use crate::utils::effects::Effects;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct ResolvedEffects {
    pub effects: Effects,
    pub reason: Option<String>,
}

impl ResolvedEffects {
    pub fn to_value(&self) -> Value {
        let mut obj = match self.effects.to_value() {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        if let Some(reason) = &self.reason {
            obj.insert("reason".to_string(), Value::String(reason.clone()));
        }
        Value::Object(obj)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResolveMode {
    Hint,
    Runtime,
}

fn effects(
    kind: &str,
    requires_apply: bool,
    irreversible: bool,
    reason: Option<String>,
) -> ResolvedEffects {
    // Flagship safety invariant: irreversible implies apply (unless explicitly disabled via a
    // higher-level contract, which we currently avoid to keep rules simple and fail-closed).
    let requires_apply = if irreversible { true } else { requires_apply };
    ResolvedEffects {
        effects: Effects {
            kind: Some(kind.to_string()),
            requires_apply,
            irreversible,
        },
        reason,
    }
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
}

fn first_token_lowercase(input: &str) -> Option<String> {
    let mut s = input.trim_start();
    // Strip leading SQL comments (best-effort).
    loop {
        if s.starts_with("--") {
            if let Some(idx) = s.find('\n') {
                s = &s[idx + 1..];
                s = s.trim_start();
                continue;
            }
            return None;
        }
        if s.starts_with("/*") {
            if let Some(idx) = s.find("*/") {
                s = &s[idx + 2..];
                s = s.trim_start();
                continue;
            }
            return None;
        }
        break;
    }
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            out.push(ch);
            if out.len() > 32 {
                break;
            }
        } else {
            break;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.to_lowercase())
    }
}

fn classify_http_method(method: &str) -> ResolvedEffects {
    let upper = method.trim().to_uppercase();
    match upper.as_str() {
        "GET" | "HEAD" | "OPTIONS" => {
            effects("read", false, false, Some(format!("http method={}", upper)))
        }
        "DELETE" => effects(
            "write",
            true,
            true,
            Some(format!("http method={} (treated as irreversible)", upper)),
        ),
        _ => effects("write", true, false, Some(format!("http method={}", upper))),
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
}

fn contains_token(haystack: &str, token: &str) -> bool {
    if haystack.is_empty() || token.is_empty() {
        return false;
    }
    let hay = haystack.as_bytes();
    let needle = token.as_bytes();
    if needle.len() > hay.len() {
        return false;
    }
    let mut offset = 0usize;
    while let Some(pos) = haystack[offset..].find(token) {
        let idx = offset + pos;
        let before_ok = idx == 0 || !is_word_byte(hay[idx - 1]);
        let after_idx = idx + needle.len();
        let after_ok = after_idx >= hay.len() || !is_word_byte(hay[after_idx]);
        if before_ok && after_ok {
            return true;
        }
        offset = idx + 1;
        if offset >= hay.len() {
            break;
        }
    }
    false
}

fn classify_sql(sql: &str) -> ResolvedEffects {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return effects("mixed", true, false, Some("sql empty".to_string()));
    }

    // Multi-statement SQL is hard to classify safely; treat as mixed and require apply.
    let mut multi_statement = false;
    if let Some(_pos) = trimmed.find(';') {
        if let Some(stripped) = trimmed.strip_suffix(';') {
            let without_trailing = stripped.trim_end();
            if without_trailing.contains(';') {
                multi_statement = true;
            }
        } else {
            multi_statement = true;
        }
    }

    let lower = trimmed.to_lowercase();
    if multi_statement {
        let irreversible = contains_token(&lower, "drop") || contains_token(&lower, "truncate");
        return effects(
            "mixed",
            true,
            irreversible,
            Some("sql contains multiple statements; treated as mixed".to_string()),
        );
    }

    let keyword = first_token_lowercase(trimmed).unwrap_or_else(|| "unknown".to_string());
    match keyword.as_str() {
        "select" | "show" | "explain" | "describe" | "desc" | "values" | "table" | "fetch" => {
            effects(
                "read",
                false,
                false,
                Some(format!("sql keyword={}", keyword)),
            )
        }

        "with" => {
            // Best-effort: if we see any write/DDL keywords anywhere, treat as write; otherwise read.
            if contains_token(&lower, "drop") || contains_token(&lower, "truncate") {
                return effects(
                    "write",
                    true,
                    true,
                    Some(
                        "sql keyword=with (contains drop/truncate; treated as irreversible)"
                            .to_string(),
                    ),
                );
            }
            let writeish = [
                "insert", "update", "delete", "merge", "create", "alter", "grant", "revoke",
                "vacuum", "analyze", "reindex", "cluster", "refresh", "call", "do", "copy",
            ];
            if writeish.iter().any(|k| contains_token(&lower, k)) {
                return effects(
                    "write",
                    true,
                    false,
                    Some(
                        "sql keyword=with (contains write/ddl keywords; treated as write)"
                            .to_string(),
                    ),
                );
            }
            effects(
                "read",
                false,
                false,
                Some("sql keyword=with (read-only heuristic)".to_string()),
            )
        }

        // Transaction/session control: safe, does not require apply.
        "begin" | "start" | "commit" | "rollback" | "savepoint" | "release" | "set" | "reset"
        | "discard" | "prepare" | "deallocate" | "declare" | "listen" | "unlisten" | "notify" => {
            effects(
                "read",
                false,
                false,
                Some(format!("sql keyword={} (session/tx control)", keyword)),
            )
        }

        "drop" | "truncate" => effects(
            "write",
            true,
            true,
            Some(format!("sql keyword={} (treated as irreversible)", keyword)),
        ),

        // DDL and maintenance.
        "alter" => effects(
            "write",
            true,
            true,
            Some("sql keyword=alter (treated as irreversible DDL)".to_string()),
        ),
        "create" | "grant" | "revoke" | "vacuum" | "analyze" | "reindex" | "cluster"
        | "refresh" | "comment" => effects(
            "write",
            true,
            false,
            Some(format!("sql keyword={}", keyword)),
        ),

        // DML.
        "insert" | "update" | "delete" | "merge" => effects(
            "write",
            true,
            false,
            Some(format!("sql keyword={}", keyword)),
        ),

        // Potentially side-effectful.
        "copy" | "call" | "do" => effects(
            "mixed",
            true,
            false,
            Some(format!("sql keyword={} (could be side-effectful)", keyword)),
        ),

        "unknown" => effects(
            "mixed",
            true,
            false,
            Some("sql keyword=unknown".to_string()),
        ),

        _ => effects(
            "write",
            true,
            false,
            Some(format!("sql keyword={}", keyword)),
        ),
    }
}

fn classify_repo_exec(command: &str, argv: &[&str]) -> ResolvedEffects {
    let cmd = command.trim().to_lowercase();
    let sub = argv
        .first()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    if cmd == "git" {
        match sub.as_str() {
            "" | "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "grep"
            | "cat-file" | "branch" | "tag" => {
                return effects(
                    "read",
                    false,
                    false,
                    Some(format!("repo exec: git {}", sub)),
                );
            }
            "push" => {
                return effects(
                    "write",
                    true,
                    true,
                    Some("repo exec: git push (treated as irreversible)".to_string()),
                );
            }
            "commit" | "merge" | "rebase" | "reset" | "checkout" | "cherry-pick" | "revert"
            | "apply" | "pull" => {
                return effects(
                    "write",
                    true,
                    false,
                    Some(format!("repo exec: git {}", sub)),
                );
            }
            _ => {
                return effects(
                    "mixed",
                    true,
                    false,
                    Some(format!(
                        "repo exec: git {} (unknown; treated as mixed)",
                        sub
                    )),
                );
            }
        }
    }

    if cmd == "kubectl" {
        match sub.as_str() {
            "" | "get" | "describe" | "diff" | "logs" | "version" | "api-resources"
            | "cluster-info" | "top" => {
                return effects(
                    "read",
                    false,
                    false,
                    Some(format!("repo exec: kubectl {}", sub)),
                );
            }
            "delete" => {
                return effects(
                    "write",
                    true,
                    true,
                    Some("repo exec: kubectl delete (treated as irreversible)".to_string()),
                );
            }
            "apply" | "patch" | "rollout" | "scale" | "annotate" | "label" | "set" => {
                return effects(
                    "write",
                    true,
                    false,
                    Some(format!("repo exec: kubectl {}", sub)),
                );
            }
            _ => {
                return effects(
                    "mixed",
                    true,
                    false,
                    Some(format!(
                        "repo exec: kubectl {} (unknown; treated as mixed)",
                        sub
                    )),
                );
            }
        }
    }

    if cmd == "helm" {
        match sub.as_str() {
            "" | "template" | "lint" | "show" | "status" | "history" | "list" => {
                return effects(
                    "read",
                    false,
                    false,
                    Some(format!("repo exec: helm {}", sub)),
                );
            }
            "install" | "upgrade" | "uninstall" | "rollback" => {
                return effects(
                    "write",
                    true,
                    false,
                    Some(format!("repo exec: helm {}", sub)),
                );
            }
            _ => {
                return effects(
                    "mixed",
                    true,
                    false,
                    Some(format!(
                        "repo exec: helm {} (unknown; treated as mixed)",
                        sub
                    )),
                );
            }
        }
    }

    if cmd == "kustomize" {
        match sub.as_str() {
            "" | "build" => {
                return effects(
                    "read",
                    false,
                    false,
                    Some(format!("repo exec: kustomize {}", sub)),
                );
            }
            _ => {
                return effects(
                    "mixed",
                    true,
                    false,
                    Some(format!(
                        "repo exec: kustomize {} (unknown; treated as mixed)",
                        sub
                    )),
                );
            }
        }
    }

    effects(
        "mixed",
        true,
        false,
        Some(format!("repo exec: {} (treated as mixed)", cmd)),
    )
}

fn resolve_tool_action_effects(
    tool: &str,
    action: &str,
    args: &Value,
    mode: ResolveMode,
) -> ResolvedEffects {
    match tool {
        "help" | "legend" => effects("read", false, false, None),

        "mcp_alias" => match action {
            "alias_get" | "alias_list" | "alias_resolve" => effects("read", false, false, None),
            "alias_upsert" => effects("write", false, false, None),
            "alias_delete" => effects(
                "write",
                false,
                true,
                Some("deletes alias (irreversible)".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_preset" => match action {
            "preset_get" | "preset_list" => effects("read", false, false, None),
            "preset_upsert" => effects("write", false, false, None),
            "preset_delete" => effects(
                "write",
                false,
                true,
                Some("deletes preset (irreversible)".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_state" => match action {
            "get" | "list" | "dump" => effects("read", false, false, None),
            "set" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on scope (session vs persistent)".to_string()),
                ),
                ResolveMode::Runtime => {
                    let scope = string_arg(args, "scope").unwrap_or("persistent");
                    if scope.eq_ignore_ascii_case("session") {
                        effects(
                            "write",
                            false,
                            false,
                            Some("state.set scope=session (ephemeral)".to_string()),
                        )
                    } else {
                        effects(
                            "write",
                            true,
                            false,
                            Some(format!("state.set scope={}", scope)),
                        )
                    }
                }
            },
            "unset" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on scope (session vs persistent)".to_string()),
                ),
                ResolveMode::Runtime => {
                    let scope = string_arg(args, "scope").unwrap_or("persistent");
                    if scope.eq_ignore_ascii_case("session") {
                        effects(
                            "write",
                            false,
                            false,
                            Some("state.unset scope=session (ephemeral)".to_string()),
                        )
                    } else {
                        effects(
                            "write",
                            true,
                            true,
                            Some(format!(
                                "state.unset scope={} (treated as irreversible)",
                                scope
                            )),
                        )
                    }
                }
            },
            "clear" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on scope (session vs persistent)".to_string()),
                ),
                ResolveMode::Runtime => {
                    let scope = string_arg(args, "scope").unwrap_or("persistent");
                    if scope.eq_ignore_ascii_case("session") {
                        effects(
                            "write",
                            false,
                            false,
                            Some("state.clear scope=session (ephemeral)".to_string()),
                        )
                    } else {
                        effects(
                            "write",
                            true,
                            true,
                            Some(format!(
                                "state.clear scope={} (treated as irreversible)",
                                scope
                            )),
                        )
                    }
                }
            },
            _ => effects("mixed", false, false, None),
        },

        "mcp_audit" => match action {
            "audit_list" | "audit_tail" | "audit_stats" => effects("read", false, false, None),
            "audit_clear" => effects(
                "write",
                false,
                true,
                Some("clears audit log (irreversible)".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_artifacts" => effects("read", false, false, None),

        "mcp_context" => effects("read", false, false, None),

        "mcp_project" => match action {
            "project_get" | "project_list" | "project_active" => {
                effects("read", false, false, None)
            }
            "project_upsert" | "project_use" | "project_unuse" => {
                effects("write", false, false, None)
            }
            "project_delete" => effects(
                "write",
                false,
                true,
                Some("deletes project binding (irreversible)".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_capability" => match action {
            "list" | "get" | "resolve" | "suggest" | "graph" | "stats" => {
                effects("read", false, false, None)
            }
            "set" | "delete" => effects(
                "read",
                false,
                false,
                Some(
                    "compatibility-only in normal mode; manifests are immutable at runtime"
                        .to_string(),
                ),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_receipt" | "mcp_profile" | "mcp_target" | "mcp_policy" => {
            effects("read", false, false, None)
        }

        "mcp_evidence" => effects("read", false, false, None),

        // Orchestrators: they compute/enforce their own effects.
        "mcp_workspace" => match action {
            "cleanup" => effects(
                "write",
                false,
                false,
                Some("workspace cleanup (in-memory)".to_string()),
            ),
            "run" => effects(
                "mixed",
                false,
                false,
                Some("workspace.run effects depend on chosen intent/runbook".to_string()),
            ),
            _ => effects("read", false, false, None),
        },

        "mcp_runbook" => match action {
            "runbook_list" | "runbook_get" => effects("read", false, false, None),
            "runbook_compile" | "runbook_upsert" | "runbook_upsert_dsl" | "runbook_delete"
            | "runbook_run_dsl" => effects(
                "read",
                false,
                false,
                Some(
                    "compatibility-only in normal mode; runbooks are manifest-backed and name-only"
                        .to_string(),
                ),
            ),
            "runbook_run" => effects(
                "mixed",
                false,
                false,
                Some("effects are determined by the runbook metadata".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_intent" => match action {
            "compile" | "dry_run" | "explain" => effects("read", false, false, None),
            "execute" => effects(
                "mixed",
                false,
                false,
                Some("effects are determined by the compiled intent plan".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_env" => match action {
            "profile_get" | "profile_list" => effects("read", false, false, None),
            "profile_upsert" => effects("write", false, false, None),
            "profile_delete" => effects(
                "write",
                false,
                true,
                Some("deletes env profile (irreversible)".to_string()),
            ),
            "write_remote" => effects("write", true, false, None),
            "run_remote" => effects("mixed", true, false, None),
            _ => effects("mixed", false, false, None),
        },

        "mcp_vault" => match action {
            "profile_get" | "profile_list" | "profile_test" => effects("read", false, false, None),
            "profile_upsert" => effects("write", false, false, None),
            "profile_delete" => effects(
                "write",
                false,
                true,
                Some("deletes vault profile (irreversible)".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_jobs" => match action {
            "job_cancel" => effects(
                "write",
                true,
                true,
                Some("cancels a job (irreversible)".to_string()),
            ),
            "job_forget" => effects(
                "write",
                false,
                false,
                Some("forgets a job locally".to_string()),
            ),
            _ => effects("read", false, false, None),
        },

        "mcp_ssh_manager" => match action {
            "profile_get" | "profile_list" | "profile_test" | "connect" | "system_info"
            | "check_host" | "sftp_list" | "sftp_exists" | "sftp_download" | "job_status"
            | "job_wait" | "job_logs_tail" | "tail_job" | "follow_job" => {
                effects("read", false, false, None)
            }
            "profile_upsert" => effects("write", false, false, None),
            "profile_delete" => effects(
                "write",
                false,
                true,
                Some("deletes ssh profile (irreversible)".to_string()),
            ),
            "authorized_keys_add" => effects(
                "write",
                true,
                true,
                Some("adds authorized key (treated as irreversible)".to_string()),
            ),
            "deploy_file" | "sftp_upload" => effects("write", true, false, None),
            "exec" | "exec_detached" | "exec_follow" | "batch" => {
                effects("mixed", true, false, None)
            }
            "job_kill" => effects(
                "write",
                true,
                true,
                Some("kills a job (irreversible)".to_string()),
            ),
            "job_forget" => effects(
                "write",
                false,
                false,
                Some("forgets a job locally".to_string()),
            ),
            _ => effects("mixed", false, false, None),
        },

        "mcp_api_client" => match action {
            "profile_get" | "profile_list" | "check" | "smoke_http" => {
                effects("read", false, false, None)
            }
            "profile_upsert" => effects("write", false, false, None),
            "profile_delete" => effects(
                "write",
                false,
                true,
                Some("deletes api profile (irreversible)".to_string()),
            ),
            "download" => match mode {
                ResolveMode::Hint => effects(
                    "write",
                    true,
                    false,
                    Some(
                        "downloads content to a local path; overwrite may be irreversible"
                            .to_string(),
                    ),
                ),
                ResolveMode::Runtime => {
                    let overwrite = bool_arg(args, "overwrite");
                    if overwrite {
                        effects(
                            "write",
                            true,
                            true,
                            Some("download overwrite=true (treated as irreversible)".to_string()),
                        )
                    } else {
                        effects(
                            "write",
                            true,
                            false,
                            Some("download writes a local file".to_string()),
                        )
                    }
                }
            },
            "request" | "paginate" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on HTTP method (GET=read, POST/PUT/PATCH=write)".to_string()),
                ),
                ResolveMode::Runtime => {
                    let method = string_arg(args, "method").unwrap_or("GET");
                    classify_http_method(method)
                }
            },
            _ => effects("mixed", false, false, None),
        },

        "mcp_psql_manager" => match action {
            "profile_get" | "profile_list" | "profile_test" => effects("read", false, false, None),
            "profile_upsert" => effects("write", false, false, None),
            "profile_delete" => effects(
                "write",
                false,
                true,
                Some("deletes postgres profile (irreversible)".to_string()),
            ),
            "select" | "count" | "exists" | "catalog_tables" | "catalog_columns"
            | "database_info" => effects("read", false, false, None),
            "export" => match mode {
                ResolveMode::Hint => effects(
                    "write",
                    true,
                    false,
                    Some(
                        "exports query results to a local file; overwrite may be irreversible"
                            .to_string(),
                    ),
                ),
                ResolveMode::Runtime => {
                    let overwrite = bool_arg(args, "overwrite");
                    if overwrite {
                        effects(
                            "write",
                            true,
                            true,
                            Some("export overwrite=true (treated as irreversible)".to_string()),
                        )
                    } else {
                        effects(
                            "write",
                            true,
                            false,
                            Some("export writes a local file".to_string()),
                        )
                    }
                }
            },
            "insert" | "insert_bulk" | "update" | "delete" => effects("write", true, false, None),
            "query" | "batch" | "transaction" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on SQL keyword (SELECT=read, DDL/DML=write)".to_string()),
                ),
                ResolveMode::Runtime => {
                    if action == "query" {
                        let sql = string_arg(args, "sql").unwrap_or("");
                        return classify_sql(sql);
                    }
                    let statements = args
                        .get("statements")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let mut any_read = false;
                    let mut any_write = false;
                    let mut irreversible = false;
                    for statement in statements.iter() {
                        let sql = statement.get("sql").and_then(|v| v.as_str()).unwrap_or("");
                        let resolved = classify_sql(sql);
                        match resolved.effects.kind.as_deref() {
                            Some("read") => any_read = true,
                            Some("write") => any_write = true,
                            _ => {
                                any_read = true;
                                any_write = true;
                            }
                        }
                        if resolved.effects.irreversible {
                            irreversible = true;
                        }
                    }
                    if any_write && any_read {
                        return effects(
                            "mixed",
                            true,
                            irreversible,
                            Some("sql batch contains mixed statements".to_string()),
                        );
                    }
                    if any_write {
                        return effects(
                            "write",
                            true,
                            irreversible,
                            Some("sql batch contains write statements".to_string()),
                        );
                    }
                    effects(
                        "read",
                        false,
                        false,
                        Some("sql batch contains read-only statements".to_string()),
                    )
                }
            },
            _ => effects("mixed", false, false, None),
        },

        "mcp_repo" => match action {
            "repo_info" | "assert_clean" | "git_diff" | "render" => {
                effects("read", false, false, None)
            }
            "apply_patch" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    false,
                    false,
                    Some("apply=false runs a check; apply=true applies the patch".to_string()),
                ),
                ResolveMode::Runtime => {
                    let apply = bool_arg(args, "apply");
                    if apply {
                        effects("write", false, false, Some("apply=true".to_string()))
                    } else {
                        effects(
                            "read",
                            false,
                            false,
                            Some("apply=false (dry-run check)".to_string()),
                        )
                    }
                }
            },
            "git_commit" | "git_revert" => effects("write", true, false, None),
            "git_push" => effects(
                "write",
                true,
                true,
                Some("git push is treated as irreversible".to_string()),
            ),
            "exec" => match mode {
                ResolveMode::Hint => effects(
                    "mixed",
                    true,
                    false,
                    Some("depends on command/subcommand".to_string()),
                ),
                ResolveMode::Runtime => {
                    let command = string_arg(args, "command").unwrap_or("");
                    let argv: Vec<&str> = args
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                        .unwrap_or_default();
                    classify_repo_exec(command, &argv)
                }
            },
            _ => effects("mixed", false, false, None),
        },

        "mcp_local" => match action {
            "fs_read" | "fs_list" | "fs_stat" => effects("read", false, false, None),
            "fs_write" | "fs_mkdir" => effects("write", true, false, None),
            "fs_rm" => effects(
                "write",
                true,
                true,
                Some("filesystem delete is treated as irreversible".to_string()),
            ),
            "exec" | "batch" => effects("mixed", true, false, None),
            _ => effects("mixed", true, false, None),
        },

        "mcp_pipeline" => match action {
            "describe" => effects("read", false, false, None),
            "run" | "deploy_smoke" => effects("mixed", true, false, None),
            _ => effects("mixed", true, false, None),
        },

        "mcp_operation" => match action {
            "observe" | "plan" | "verify" | "status" | "list" => {
                effects("read", false, false, None)
            }
            "apply" => effects("write", true, false, None),
            "rollback" => effects(
                "write",
                true,
                true,
                Some("operation rollback is treated as potentially irreversible".to_string()),
            ),
            "cancel" => effects("write", true, false, None),
            _ => effects("mixed", true, false, None),
        },

        _ => effects("mixed", false, false, None),
    }
}

pub fn resolve_tool_call_effects(tool: &str, args: &Value) -> ResolvedEffects {
    let canonical = canonical_tool_name(tool);
    let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
    resolve_tool_action_effects(canonical, action, args, ResolveMode::Runtime)
}

pub fn resolve_tool_call_effects_for_result(
    tool: &str,
    args: &Value,
    result: &Value,
) -> ResolvedEffects {
    let canonical = canonical_tool_name(tool);
    // Prefer explicit effects returned by the tool.
    if let Some(obj) = result.get("effects").and_then(|v| v.as_object()) {
        return parse_effects_object(obj)
            .unwrap_or_else(|| resolve_tool_call_effects(canonical, args));
    }
    if canonical == "mcp_intent" {
        if let Some(obj) = result
            .get("plan")
            .and_then(|v| v.get("effects"))
            .and_then(|v| v.as_object())
        {
            return parse_effects_object(obj)
                .unwrap_or_else(|| resolve_tool_call_effects(canonical, args));
        }
    }
    if canonical == "mcp_operation" {
        if let Some(obj) = result
            .get("operation")
            .and_then(|v| v.get("effects"))
            .and_then(|v| v.as_object())
        {
            return parse_effects_object(obj)
                .unwrap_or_else(|| resolve_tool_call_effects(canonical, args));
        }
    }
    resolve_tool_call_effects(canonical, args)
}

pub fn hint_effects_for_tool_action(tool: &str, action: &str) -> ResolvedEffects {
    let canonical = canonical_tool_name(tool);
    resolve_tool_action_effects(canonical, action, &Value::Null, ResolveMode::Hint)
}

fn parse_effects_object(obj: &serde_json::Map<String, Value>) -> Option<ResolvedEffects> {
    let kind = obj
        .get("kind")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let requires_apply = obj
        .get("requires_apply")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let irreversible = obj
        .get("irreversible")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(ResolvedEffects {
        effects: Effects {
            kind,
            requires_apply,
            irreversible,
        },
        reason: None,
    })
}
