use crate::errors::ToolError;
use crate::utils::suggest::suggest;
use serde_json::Value;

pub fn unknown_action_error(
    tool: &str,
    action: Option<&Value>,
    known_actions: &[&str],
) -> ToolError {
    let action_value = action
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| String::from(""));
    let known: Vec<String> = known_actions.iter().map(|s| s.to_string()).collect();
    let suggestions = if !action_value.is_empty() {
        suggest(&action_value, &known, 5)
    } else {
        Vec::new()
    };
    let shown: Vec<String> = known.iter().take(24).cloned().collect();
    let suffix = if known.len() > shown.len() {
        ", ..."
    } else {
        ""
    };
    let list_hint = if !shown.is_empty() {
        format!("Use one of: {}{}.", shown.join(", "), suffix)
    } else {
        String::new()
    };
    let did_you_mean = if !suggestions.is_empty() {
        format!("Did you mean: {}?", suggestions.join(", "))
    } else {
        String::new()
    };
    let hint = [did_you_mean, list_hint]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let mut err = ToolError::invalid_params(format!("Unknown {} action: {}", tool, action_value));
    if !hint.is_empty() {
        err = err.with_hint(hint);
    }
    if !known.is_empty() {
        err = err.with_details(serde_json::json!({
            "known_actions": known,
            "did_you_mean": suggestions,
        }));
    }
    err
}
