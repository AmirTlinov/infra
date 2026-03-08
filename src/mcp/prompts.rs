use serde_json::Value;

fn prompt_entry(name: &str, description: &str, arguments: &[(&str, &str, bool)]) -> Value {
    let args = arguments
        .iter()
        .map(|(name, description, required)| {
            serde_json::json!({
                "name": name,
                "description": description,
                "required": required,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "name": name,
        "description": description,
        "arguments": args,
    })
}

fn text_arg(args: &Value, key: &str, fallback: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

pub fn list_prompts() -> Value {
    serde_json::json!({
        "prompts": [
            prompt_entry(
                "deploy_service",
                "Plan a safe deploy via mcp_target → mcp_profile/mcp_policy → mcp_operation/mcp_receipt.",
                &[("project", "Project name when target resolution depends on project bindings.", false), ("target", "Target or environment name.", true), ("capability", "Capability family to use, for example gitops.release.", false)]
            ),
            prompt_entry(
                "rollback_release",
                "Plan a rollback with target/profile/policy checks, evidence, and post-rollback verification.",
                &[("project", "Project name when target resolution depends on project bindings.", false), ("target", "Target or environment name.", true), ("capability", "Capability family to use, for example gitops.rollback.", false)]
            ),
            prompt_entry(
                "diagnose_incident",
                "Drive an incident diagnosis using resources first, then mcp_target/mcp_receipt before raw expert tools.",
                &[("project", "Project name when target resolution depends on project bindings.", false), ("target", "Target or environment name.", false), ("symptom", "Observed problem or failure signal.", true)]
            ),
            prompt_entry(
                "bootstrap_target",
                "Bootstrap a new target with explicit target/profile/policy contracts and receipt expectations.",
                &[("project", "Project that will own the target bindings.", false), ("target", "Target or environment name.", true)]
            )
        ]
    })
}

pub fn get_prompt(name: &str, arguments: &Value) -> Option<Value> {
    let project = text_arg(arguments, "project", "<project>");
    let target = text_arg(arguments, "target", "<target>");
    let capability = text_arg(arguments, "capability", "<capability>");
    let symptom = text_arg(arguments, "symptom", "<symptom>");

    let (description, text) = match name {
        "deploy_service" => (
            "Deploy a service through canonical capability and operation flows.",
            format!(
                "Start with infra://surface/core plus infra://schemas/mcp_target, infra://schemas/mcp_profile, infra://schemas/mcp_policy, and infra://schemas/mcp_receipt. Inspect target '{target}' in project '{project}' through mcp_target, inspect referenced profiles through mcp_profile, resolve or evaluate effective change policy through mcp_policy, then prefer capability '{capability}' if it is set or resolve the best deploy/apply capability from the capabilities resource. Drive the change through mcp_operation (observe -> plan -> apply -> verify). After every write, inspect and quote the resulting receipt through mcp_receipt. Fall back to raw expert tools only if the canonical surface cannot answer the question."
            ),
        ),
        "rollback_release" => (
            "Rollback a release with verification and receipts.",
            format!(
                "Prepare a rollback for target '{target}' in project '{project}' using capability '{capability}' when provided. Inspect the target via mcp_target, confirm the effective policy via mcp_policy, inspect recent receipts via infra://receipts/recent or mcp_receipt, then execute the minimum required rollback through mcp_operation. Verify post-rollback health and quote the final receipt before reporting done."
            ),
        ),
        "diagnose_incident" => (
            "Diagnose an incident with read-first behavior.",
            format!(
                "Diagnose the incident for target '{target}' in project '{project}' with symptom '{symptom}'. Start with infra://surface/core, inspect the target via mcp_target, inspect recent receipts via mcp_receipt or infra://receipts/recent, and use mcp_profile to see which provider bindings are in play before touching raw expert tools. Narrow the hypothesis before any write action, and end with a concise evidence-backed explanation plus the next safest action."
            ),
        ),
        "bootstrap_target" => (
            "Bootstrap a target with explicit policy and receipt expectations.",
            format!(
                "Bootstrap target '{target}' in project '{project}'. Define the target contract expected by mcp_target, enumerate the named provider profiles that mcp_profile should expose, specify the effective change policy that mcp_policy must surface, and sketch the first observe/verify/apply loop plus the receipt fields that must be captured after writes. Keep the flow capability-first, operation-first, and receipt-driven; use raw expert managers only as backing details, not as the primary narrative."
            ),
        ),
        _ => return None,
    };

    Some(serde_json::json!({
        "description": description,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": text,
                    }
                ]
            }
        ]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_catalog_includes_deploy_and_bootstrap() {
        let prompts = list_prompts();
        let names = prompts
            .get("prompts")
            .and_then(|v| v.as_array())
            .expect("prompts array")
            .iter()
            .filter_map(|v| v.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();
        assert!(names.contains(&"deploy_service"));
        assert!(names.contains(&"bootstrap_target"));
    }

    #[test]
    fn prompt_get_renders_argument_values() {
        let prompt = get_prompt(
            "diagnose_incident",
            &serde_json::json!({ "project": "myapp", "target": "prod", "symptom": "5xx spike" }),
        )
        .expect("prompt");
        let text = prompt
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(text.contains("myapp"));
        assert!(text.contains("prod"));
        assert!(text.contains("5xx spike"));
    }

    #[test]
    fn deploy_prompt_mentions_new_canonical_surface() {
        let prompt = get_prompt(
            "deploy_service",
            &serde_json::json!({
                "project": "myapp",
                "target": "prod",
                "capability": "gitops.release"
            }),
        )
        .expect("prompt");
        let text = prompt
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("content"))
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(text.contains("mcp_target"));
        assert!(text.contains("mcp_profile"));
        assert!(text.contains("mcp_policy"));
        assert!(text.contains("mcp_receipt"));
        assert!(text.contains("myapp"));
        assert!(text.contains("prod"));
    }
}
