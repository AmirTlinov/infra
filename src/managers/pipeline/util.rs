use crate::utils::feature_flags::is_truthy;
use serde_json::Value;

pub(crate) fn read_positive_int(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    if value.is_null() {
        return None;
    }
    let parsed = if let Some(v) = value.as_u64() {
        Some(v)
    } else if let Some(v) = value.as_i64() {
        if v > 0 {
            Some(v as u64)
        } else {
            None
        }
    } else if let Some(text) = value.as_str() {
        text.trim().parse::<u64>().ok()
    } else {
        value.to_string().trim().parse::<u64>().ok()
    };
    parsed.filter(|v| *v > 0).map(|v| v.max(1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StreamToArtifactMode {
    Full,
    Capped,
}

pub(crate) fn resolve_stream_to_artifact_mode() -> Option<StreamToArtifactMode> {
    let raw = std::env::var("INFRA_PIPELINE_STREAM_TO_ARTIFACT")
        .or_else(|_| std::env::var("INFRA_STREAM_TO_ARTIFACT"))
        .ok();
    let raw = raw?;
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized == "full" {
        return Some(StreamToArtifactMode::Full);
    }
    if normalized == "capped" {
        return Some(StreamToArtifactMode::Capped);
    }
    if is_truthy(&normalized) {
        return Some(StreamToArtifactMode::Capped);
    }
    None
}

pub(crate) fn resolve_max_capture_bytes() -> usize {
    let raw = std::env::var("INFRA_PIPELINE_MAX_CAPTURE_BYTES")
        .or_else(|_| std::env::var("INFRA_MAX_CAPTURE_BYTES"))
        .ok();
    raw.and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(256 * 1024)
}
