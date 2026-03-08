use serde_json::Value;

pub fn build_tool_example(tool_name: &str, action_name: &str) -> Option<Value> {
    if tool_name.trim().is_empty() || action_name.trim().is_empty() {
        return None;
    }

    if tool_name == "mcp_runbook" {
        match action_name {
            "runbook_list" => {
                return Some(serde_json::json!({
                    "action": "runbook_list",
                    "limit": 50,
                    "query": "k8s",
                    "tags": ["k8s"],
                }));
            }
            "runbook_get" => {
                return Some(serde_json::json!({
                    "action": "runbook_get",
                    "name": "k8s.diff"
                }));
            }
            "runbook_run" => {
                return Some(serde_json::json!({
                    "action": "runbook_run",
                    "name": "k8s.diff",
                    "input": { "namespace": "default" },
                }));
            }
            "runbook_upsert" => {
                return Some(serde_json::json!({
                    "action": "runbook_upsert",
                    "name": "deploy.preview",
                    "runbook": {
                        "description": "Compatibility-only example: move this entry into .infra/runbooks.json and execute by name.",
                        "tags": ["gitops"],
                        "steps": [
                            { "tool": "mcp_repo", "args": { "action": "render", "repo_root": "/repo", "chart": "./chart" } }
                        ]
                    }
                }));
            }
            "runbook_upsert_dsl" => {
                return Some(serde_json::json!({
                    "action": "runbook_upsert_dsl",
                    "name": "deploy.preview",
                    "text": "tool mcp_repo
action render
arg repo_root=/repo
arg chart=./chart"
                }));
            }
            "runbook_run_dsl" => {
                return Some(serde_json::json!({
                    "action": "runbook_run_dsl",
                    "text": "tool mcp_repo
action render
arg repo_root=/repo
arg chart=./chart",
                    "input": {}
                }));
            }
            "runbook_compile" => {
                return Some(serde_json::json!({
                    "action": "runbook_compile",
                    "text": "tool mcp_repo
action render
arg repo_root=/repo
arg chart=./chart"
                }));
            }
            "runbook_delete" => {
                return Some(serde_json::json!({
                    "action": "runbook_delete",
                    "name": "deploy.preview"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_capability" {
        match action_name {
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "query": "k8s",
                    "tags": ["gitops"],
                    "limit": 25
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "name": "k8s.diff"
                }));
            }
            "families" => {
                return Some(serde_json::json!({
                    "action": "families"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_operation" {
        match action_name {
            "observe" => {
                return Some(serde_json::json!({
                    "action": "observe",
                    "family": "gitops",
                    "target": "prod"
                }));
            }
            "plan" => {
                return Some(serde_json::json!({
                    "action": "plan",
                    "family": "gitops",
                    "target": "prod",
                    "input": { "repo_root": "/repo" }
                }));
            }
            "apply" => {
                return Some(serde_json::json!({
                    "action": "apply",
                    "family": "gitops",
                    "target": "prod",
                    "apply": true,
                    "input": { "repo_root": "/repo" }
                }));
            }
            "verify" => {
                return Some(serde_json::json!({
                    "action": "verify",
                    "family": "gitops",
                    "target": "prod",
                    "input": { "repo_root": "/repo" }
                }));
            }
            "rollback" => {
                return Some(serde_json::json!({
                    "action": "rollback",
                    "family": "gitops",
                    "target": "prod",
                    "apply": true,
                    "confirm": true,
                    "input": { "repo_root": "/repo" }
                }));
            }
            "status" => {
                return Some(serde_json::json!({
                    "action": "status",
                    "operation_id": "<operation_id>"
                }));
            }
            "cancel" => {
                return Some(serde_json::json!({
                    "action": "cancel",
                    "operation_id": "<operation_id>",
                    "apply": true
                }));
            }
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "limit": 20
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_receipt" {
        match action_name {
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "status": "applied",
                    "limit": 10
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "operation_id": "op_01JDEPLOYEXAMPLE"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_policy" {
        match action_name {
            "resolve" => {
                return Some(serde_json::json!({
                    "action": "resolve",
                    "project": "myapp",
                    "target": "prod",
                    "inputs": {
                        "policy_profile_name": "operatorless"
                    }
                }));
            }
            "evaluate" => {
                return Some(serde_json::json!({
                    "action": "evaluate",
                    "intent": "gitops.release",
                    "project": "myapp",
                    "target": "prod",
                    "inputs": {
                        "policy_profile_name": "operatorless",
                        "merge": false
                    }
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_profile" {
        match action_name {
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "type": "ssh"
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "name": "myapp-prod-ssh"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_target" {
        match action_name {
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "project": "myapp"
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "project": "myapp",
                    "name": "prod"
                }));
            }
            "resolve" => {
                return Some(serde_json::json!({
                    "action": "resolve",
                    "project": "myapp"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_evidence" {
        match action_name {
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "query": "evidence-",
                    "limit": 50,
                    "offset": 0
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "id": "evidence-2025-01-01-abc123.json"
                }));
            }
            _ => {}
        }
    }

    if tool_name == "mcp_state" {
        match action_name {
            "set" => {
                // Safe-by-default example: prefer session scope (ephemeral) to avoid persistent writes.
                return Some(serde_json::json!({
                    "action": "set",
                    "scope": "session",
                    "key": "example.key",
                    "value": { "ok": true }
                }));
            }
            "get" => {
                return Some(serde_json::json!({
                    "action": "get",
                    "scope": "session",
                    "key": "example.key"
                }));
            }
            "list" => {
                return Some(serde_json::json!({
                    "action": "list",
                    "scope": "session",
                    "prefix": "example."
                }));
            }
            "unset" => {
                return Some(serde_json::json!({
                    "action": "unset",
                    "scope": "session",
                    "key": "example.key"
                }));
            }
            "clear" => {
                return Some(serde_json::json!({
                    "action": "clear",
                    "scope": "session"
                }));
            }
            "dump" => {
                return Some(serde_json::json!({
                    "action": "dump",
                    "scope": "session"
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_ssh_manager" {
        match action_name {
            "profile_upsert" => {
                return Some(serde_json::json!({
                    "action": "profile_upsert",
                    "profile_name": "my-ssh",
                    "connection": { "host": "example.com", "port": 22, "username": "root", "private_key_path": "~/.ssh/id_ed25519", "host_key_policy": "tofu" },
                }));
            }
            "authorized_keys_add" => {
                return Some(serde_json::json!({
                    "action": "authorized_keys_add",
                    "target": "prod",
                    "public_key_path": "~/.ssh/id_ed25519.pub",
                }));
            }
            "exec" => {
                return Some(serde_json::json!({
                    "action": "exec",
                    "target": "prod",
                    "command": "uname -a",
                }));
            }
            "exec_follow" => {
                return Some(serde_json::json!({
                    "action": "exec_follow",
                    "target": "prod",
                    "command": "sleep 60 && echo done",
                    "timeout_ms": 600000,
                    "lines": 120,
                }));
            }
            "exec_detached" => {
                return Some(serde_json::json!({
                    "action": "exec_detached",
                    "target": "prod",
                    "command": "sleep 60 && echo done",
                    "log_path": "/tmp/infra-detached.log",
                }));
            }
            "deploy_file" => {
                return Some(serde_json::json!({
                    "action": "deploy_file",
                    "target": "prod",
                    "local_path": "./build/app.bin",
                    "remote_path": "/opt/myapp/app.bin",
                    "overwrite": true,
                    "restart": "myapp",
                }));
            }
            "tail_job" => {
                return Some(serde_json::json!({
                    "action": "tail_job",
                    "job_id": "<job_id>",
                    "lines": 120,
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_project" {
        match action_name {
            "project_upsert" => {
                return Some(serde_json::json!({
                    "action": "project_upsert",
                    "name": "myapp",
                    "project": {
                        "default_target": "prod",
                        "targets": {
                            "prod": {
                                "ssh_profile": "myapp-prod-ssh",
                                "env_profile": "myapp-prod-env",
                                "postgres_profile": "myapp-prod-db",
                                "api_profile": "myapp-prod-api",
                                "cwd": "/opt/myapp",
                                "env_path": "/opt/myapp/.env",
                            }
                        }
                    }
                }));
            }
            "project_use" => {
                return Some(serde_json::json!({
                    "action": "project_use",
                    "name": "myapp",
                    "scope": "persistent",
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_context" {
        match action_name {
            "summary" => {
                return Some(
                    serde_json::json!({"action": "summary", "project": "myapp", "target": "prod"}),
                );
            }
            "refresh" => {
                return Some(serde_json::json!({"action": "refresh", "cwd": "/srv/myapp"}));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_workspace" {
        match action_name {
            "summary" => {
                return Some(
                    serde_json::json!({"action": "summary", "project": "myapp", "target": "prod"}),
                );
            }
            "diagnose" => return Some(serde_json::json!({"action": "diagnose"})),
            "run" => {
                return Some(
                    serde_json::json!({"action": "run", "intent_type": "k8s.diff", "inputs": {"overlay": "/repo/overlays/prod"}}),
                );
            }
            "cleanup" => return Some(serde_json::json!({"action": "cleanup"})),
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_env" {
        match action_name {
            "profile_upsert" => {
                return Some(serde_json::json!({
                    "action": "profile_upsert",
                    "profile_name": "myapp-prod-env",
                    "secrets": { "DATABASE_URL": "ref:vault:kv2:secret/myapp/prod#DATABASE_URL" },
                }));
            }
            "write_remote" => {
                return Some(serde_json::json!({
                    "action": "write_remote",
                    "target": "prod",
                    "overwrite": false,
                    "backup": true,
                }));
            }
            "run_remote" => {
                return Some(serde_json::json!({
                    "action": "run_remote",
                    "target": "prod",
                    "command": "printenv | head",
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_vault" {
        match action_name {
            "profile_upsert" => {
                return Some(serde_json::json!({
                    "action": "profile_upsert",
                    "profile_name": "corp-vault",
                    "addr": "https://vault.example.com",
                    "namespace": "team-a",
                    "auth_type": "approle",
                    "role_id": "<role_id>",
                    "secret_id": "<secret_id>",
                }));
            }
            "profile_test" => {
                return Some(serde_json::json!({
                    "action": "profile_test",
                    "profile_name": "corp-vault",
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_psql_manager" {
        match action_name {
            "query" => {
                return Some(
                    serde_json::json!({"action": "query", "target": "prod", "sql": "SELECT 1"}),
                );
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_api_client" {
        match action_name {
            "request" => {
                return Some(serde_json::json!({
                    "action": "request",
                    "target": "prod",
                    "method": "GET",
                    "url": "/health",
                }));
            }
            "smoke_http" => {
                return Some(serde_json::json!({
                    "action": "smoke_http",
                    "url": "https://example.com/healthz",
                    "expect_code": 200,
                    "follow_redirects": true,
                    "insecure_ok": true,
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_repo" {
        match action_name {
            "repo_info" => {
                return Some(serde_json::json!({"action": "repo_info", "repo_root": "/repo"}));
            }
            "assert_clean" => {
                return Some(serde_json::json!({"action": "assert_clean", "repo_root": "/repo"}));
            }
            "exec" => {
                return Some(serde_json::json!({
                    "action": "exec",
                    "repo_root": "/repo",
                    "command": "git",
                    "args": ["status", "--short"],
                }));
            }
            "apply_patch" => {
                let patch = [
                    "*** Begin Patch",
                    "*** Add File: hello.txt",
                    "+Hello",
                    "*** End Patch",
                ]
                .join("\n")
                    + "\n";
                return Some(serde_json::json!({
                    "action": "apply_patch",
                    "repo_root": "/repo",
                    "apply": true,
                    "patch": patch,
                }));
            }
            "git_commit" => {
                return Some(serde_json::json!({
                    "action": "git_commit",
                    "repo_root": "/repo",
                    "apply": true,
                    "message": "chore(gitops): update manifests",
                }));
            }
            "git_revert" => {
                return Some(serde_json::json!({
                    "action": "git_revert",
                    "repo_root": "/repo",
                    "apply": true,
                    "sha": "HEAD",
                }));
            }
            "git_push" => {
                return Some(serde_json::json!({
                    "action": "git_push",
                    "repo_root": "/repo",
                    "apply": true,
                    "remote": "origin",
                    "branch": "sf/gitops/update-123",
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name, "repo_root": "/repo"})),
        }
    }

    if tool_name == "mcp_artifacts" {
        match action_name {
            "get" => {
                return Some(
                    serde_json::json!({"action": "get", "uri": "artifact://runs/<trace>/tool_calls/<span>/result.json", "max_bytes": 16384, "encoding": "utf8"}),
                );
            }
            "head" => {
                return Some(
                    serde_json::json!({"action": "head", "uri": "artifact://runs/<trace>/tool_calls/<span>/stdout.log", "max_bytes": 16384, "encoding": "utf8"}),
                );
            }
            "tail" => {
                return Some(
                    serde_json::json!({"action": "tail", "uri": "artifact://runs/<trace>/tool_calls/<span>/stdout.log", "max_bytes": 16384, "encoding": "utf8"}),
                );
            }
            "list" => {
                return Some(
                    serde_json::json!({"action": "list", "prefix": "runs/<trace>/tool_calls/<span>/", "limit": 50}),
                );
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_jobs" {
        match action_name {
            "follow_job" => {
                return Some(
                    serde_json::json!({"action": "follow_job", "job_id": "<job_id>", "timeout_ms": 600000, "lines": 120}),
                );
            }
            "tail_job" => {
                return Some(
                    serde_json::json!({"action": "tail_job", "job_id": "<job_id>", "lines": 120}),
                );
            }
            "job_status" => {
                return Some(serde_json::json!({"action": "job_status", "job_id": "<job_id>"}));
            }
            "job_cancel" => {
                return Some(serde_json::json!({"action": "job_cancel", "job_id": "<job_id>"}));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    if tool_name == "mcp_intent" {
        match action_name {
            "compile" => {
                return Some(serde_json::json!({
                    "action": "compile",
                    "intent": { "type": "k8s.diff", "inputs": { "overlay": "/repo/overlay" } },
                }));
            }
            "execute" => {
                return Some(serde_json::json!({
                    "action": "execute",
                    "apply": true,
                    "intent": { "type": "k8s.apply", "inputs": { "overlay": "/repo/overlay" } },
                }));
            }
            _ => return Some(serde_json::json!({"action": action_name})),
        }
    }

    Some(serde_json::json!({"action": action_name}))
}
