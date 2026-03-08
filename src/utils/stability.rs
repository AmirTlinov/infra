use crate::errors::{ToolError, ToolErrorKind};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StabilityClassification {
    None,
    Transient,
    Permanent,
    CircuitOpen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StabilityMode {
    Off,
    Auto,
    Aggressive,
}

#[derive(Clone, Copy, Debug)]
pub struct StabilityPreset {
    pub max_attempts: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: f64,
    pub circuit_open_ms: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct StabilityDefaults {
    pub auto: StabilityPreset,
    pub aggressive: StabilityPreset,
}

#[derive(Clone, Debug)]
pub struct StabilityPolicy {
    pub enabled: bool,
    pub mode: StabilityMode,
    pub max_attempts: usize,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: f64,
    pub circuit_open_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StabilityMeta {
    pub retried: bool,
    pub attempts: usize,
    pub classification: StabilityClassification,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_retry_after_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug_ref: Option<Value>,
}

impl StabilityDefaults {
    pub fn policy_for_mode(&self, mode: StabilityMode) -> StabilityPolicy {
        match mode {
            StabilityMode::Off => StabilityPolicy {
                enabled: false,
                mode,
                max_attempts: 1,
                base_delay_ms: 0,
                max_delay_ms: 0,
                jitter: 0.0,
                circuit_open_ms: 0,
            },
            StabilityMode::Auto => StabilityPolicy {
                enabled: true,
                mode,
                max_attempts: self.auto.max_attempts.max(1),
                base_delay_ms: self.auto.base_delay_ms,
                max_delay_ms: self.auto.max_delay_ms.max(self.auto.base_delay_ms),
                jitter: self.auto.jitter.clamp(0.0, 1.0),
                circuit_open_ms: self.auto.circuit_open_ms,
            },
            StabilityMode::Aggressive => StabilityPolicy {
                enabled: true,
                mode,
                max_attempts: self.aggressive.max_attempts.max(1),
                base_delay_ms: self.aggressive.base_delay_ms,
                max_delay_ms: self
                    .aggressive
                    .max_delay_ms
                    .max(self.aggressive.base_delay_ms),
                jitter: self.aggressive.jitter.clamp(0.0, 1.0),
                circuit_open_ms: self.aggressive.circuit_open_ms,
            },
        }
    }
}

impl StabilityMeta {
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

pub fn parse_mode(value: Option<&Value>) -> Option<StabilityMode> {
    let value = value?;
    if let Some(flag) = value.as_bool() {
        return if flag {
            Some(StabilityMode::Auto)
        } else {
            Some(StabilityMode::Off)
        };
    }
    let raw = value
        .as_str()
        .unwrap_or(&value.to_string())
        .trim()
        .to_lowercase();
    match raw.as_str() {
        "off" | "false" | "0" => Some(StabilityMode::Off),
        "auto" | "on" | "true" | "1" => Some(StabilityMode::Auto),
        "aggressive" | "high" => Some(StabilityMode::Aggressive),
        _ => None,
    }
}

pub fn apply_stability_source(
    policy: &mut StabilityPolicy,
    source: Option<&Value>,
    defaults: StabilityDefaults,
) {
    let Some(source) = source else {
        return;
    };

    if source.is_string() || source.is_boolean() {
        if let Some(mode) = parse_mode(Some(source)) {
            *policy = defaults.policy_for_mode(mode);
        }
        return;
    }

    let Some(obj) = source.as_object() else {
        return;
    };

    if let Some(mode) = parse_mode(obj.get("mode")) {
        *policy = defaults.policy_for_mode(mode);
    }

    if let Some(enabled) = obj.get("enabled").and_then(|v| v.as_bool()) {
        policy.enabled = enabled;
    }
    if let Some(max_attempts) = obj.get("max_attempts").and_then(|v| v.as_u64()) {
        policy.max_attempts = (max_attempts as usize).max(1);
    }
    if let Some(base_delay_ms) = obj.get("base_delay_ms").and_then(|v| v.as_u64()) {
        policy.base_delay_ms = base_delay_ms;
    }
    if let Some(max_delay_ms) = obj.get("max_delay_ms").and_then(|v| v.as_u64()) {
        policy.max_delay_ms = max_delay_ms.max(policy.base_delay_ms);
    }
    if let Some(jitter) = obj.get("jitter").and_then(|v| v.as_f64()) {
        policy.jitter = jitter.clamp(0.0, 1.0);
    }
    if let Some(circuit_open_ms) = obj.get("circuit_open_ms").and_then(|v| v.as_u64()) {
        policy.circuit_open_ms = circuit_open_ms;
    }

    if policy.max_attempts <= 1 {
        policy.max_attempts = 1;
        policy.enabled = false;
    }
    if policy.max_delay_ms < policy.base_delay_ms {
        policy.max_delay_ms = policy.base_delay_ms;
    }
}

pub fn compute_backoff_delay_ms(
    attempt: usize,
    base_delay_ms: u64,
    max_delay_ms: u64,
    jitter: f64,
) -> u64 {
    if base_delay_ms == 0 {
        return 0;
    }

    let factor: f64 = 2.0;
    let mut delay = (base_delay_ms as f64) * factor.powi((attempt.saturating_sub(1)) as i32);
    let max_delay = max_delay_ms.max(base_delay_ms) as f64;
    if delay > max_delay {
        delay = max_delay;
    }
    if jitter > 0.0 {
        let bounded_jitter = jitter.clamp(0.0, 1.0);
        let delta = delay * bounded_jitter;
        delay = delay - delta + rand::random::<f64>() * delta * 2.0;
    }
    delay.max(0.0) as u64
}

pub fn classify_tool_error(error: &ToolError) -> StabilityClassification {
    match error.kind {
        ToolErrorKind::Timeout | ToolErrorKind::Retryable => StabilityClassification::Transient,
        ToolErrorKind::InvalidParams
        | ToolErrorKind::Denied
        | ToolErrorKind::NotFound
        | ToolErrorKind::Conflict => StabilityClassification::Permanent,
        ToolErrorKind::Internal => classify_message(&error.message),
    }
}

pub fn classify_message(message: &str) -> StabilityClassification {
    let text = message.to_lowercase();
    let transient_markers = [
        "timeout",
        "timed out",
        "connection reset",
        "connection refused",
        "broken pipe",
        "unexpected eof",
        "network",
        "temporary failure",
        "temporarily unavailable",
        "dns",
        "tls",
        "handshake",
        "would block",
        "connection aborted",
    ];
    if transient_markers.iter().any(|needle| text.contains(needle)) {
        return StabilityClassification::Transient;
    }
    StabilityClassification::Permanent
}

pub fn should_emit_stability(meta: &StabilityMeta, debug_requested: bool) -> bool {
    debug_requested
        || meta.retried
        || meta.classification == StabilityClassification::CircuitOpen
        || meta.classification == StabilityClassification::Transient
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parser_accepts_string_and_bool() {
        assert_eq!(
            parse_mode(Some(&Value::String("auto".to_string()))),
            Some(StabilityMode::Auto)
        );
        assert_eq!(
            parse_mode(Some(&Value::String("aggressive".to_string()))),
            Some(StabilityMode::Aggressive)
        );
        assert_eq!(
            parse_mode(Some(&Value::Bool(false))),
            Some(StabilityMode::Off)
        );
    }

    #[test]
    fn apply_stability_source_overrides_mode_and_fields() {
        let defaults = StabilityDefaults {
            auto: StabilityPreset {
                max_attempts: 3,
                base_delay_ms: 250,
                max_delay_ms: 2_000,
                jitter: 0.2,
                circuit_open_ms: 5_000,
            },
            aggressive: StabilityPreset {
                max_attempts: 5,
                base_delay_ms: 200,
                max_delay_ms: 3_000,
                jitter: 0.3,
                circuit_open_ms: 10_000,
            },
        };
        let mut policy = defaults.policy_for_mode(StabilityMode::Auto);
        apply_stability_source(
            &mut policy,
            Some(&serde_json::json!({
                "mode": "off",
                "enabled": true,
                "max_attempts": 4,
                "base_delay_ms": 100,
                "max_delay_ms": 900,
                "jitter": 0.5,
                "circuit_open_ms": 7000
            })),
            defaults,
        );
        assert!(policy.enabled);
        assert_eq!(policy.mode, StabilityMode::Off);
        assert_eq!(policy.max_attempts, 4);
        assert_eq!(policy.base_delay_ms, 100);
        assert_eq!(policy.max_delay_ms, 900);
        assert_eq!(policy.circuit_open_ms, 7_000);
    }

    #[test]
    fn classify_message_marks_transient_network_errors() {
        assert_eq!(
            classify_message("Failed to connect SSH: connection reset by peer"),
            StabilityClassification::Transient
        );
        assert_eq!(
            classify_message("host key mismatch"),
            StabilityClassification::Permanent
        );
    }

    #[test]
    fn compute_backoff_respects_bounds_when_no_jitter() {
        assert_eq!(compute_backoff_delay_ms(1, 250, 5_000, 0.0), 250);
        assert_eq!(compute_backoff_delay_ms(2, 250, 5_000, 0.0), 500);
        assert_eq!(compute_backoff_delay_ms(6, 250, 1_000, 0.0), 1_000);
    }
}
