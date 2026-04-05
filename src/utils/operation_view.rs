use crate::services::job::JobService;
use serde_json::Value;

pub fn derive_live_operation(receipt: &Value, job_service: &JobService) -> Value {
    let mut out = receipt.clone();
    let job_ids = receipt
        .get("job_ids")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|text| text.to_string()))
        .collect::<Vec<_>>();
    let job_outcomes = job_ids
        .iter()
        .map(|job_id| {
            let job = job_service.get(job_id).unwrap_or(Value::Null);
            serde_json::json!({
                "job_id": job_id,
                "status": canonical_job_status(job.get("status").and_then(|value| value.as_str())),
                "raw_status": job.get("status").cloned().unwrap_or(Value::Null),
                "started_at": job.get("started_at").cloned().unwrap_or(Value::Null),
                "updated_at": job.get("updated_at").cloned().unwrap_or(Value::Null),
                "ended_at": job.get("ended_at").cloned().unwrap_or(Value::Null),
                "artifacts": job.get("artifacts").cloned().unwrap_or(Value::Null),
                "error": job.get("error").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();

    let mut live_status = receipt
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown")
        .to_string();
    let base_success = receipt
        .get("result_success")
        .or_else(|| receipt.get("success"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mut live_success = receipt
        .get("success")
        .and_then(|value| value.as_bool())
        .unwrap_or(base_success);

    let action = receipt
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or("");

    let any_pending = job_outcomes.iter().any(|job| {
        matches!(
            job.get("status").and_then(|value| value.as_str()),
            Some("running") | Some("waiting_external")
        )
    });
    let any_failed = job_outcomes.iter().any(|job| {
        matches!(
            job.get("status").and_then(|value| value.as_str()),
            Some("failed")
        )
    });
    let verification_passed = receipt
        .get("verification")
        .and_then(|value| value.get("passed"))
        .and_then(|value| value.as_bool());

    if action == "plan" {
        // keep planned/blocked semantics
    } else if any_pending {
        live_status = "waiting_external".to_string();
        live_success = false;
    } else if any_failed {
        live_status = "failed".to_string();
        live_success = false;
    } else if action == "verify" && verification_passed == Some(false) {
        live_status = "verify_failed".to_string();
        live_success = false;
    } else if base_success {
        live_status = "completed".to_string();
        live_success = true;
    } else if !matches!(live_status.as_str(), "planned" | "blocked") {
        live_status = "failed".to_string();
        live_success = false;
    }

    if let Value::Object(map) = &mut out {
        map.insert("status".to_string(), Value::String(live_status));
        map.insert("success".to_string(), Value::Bool(live_success));
        map.entry("result_success".to_string())
            .or_insert(Value::Bool(base_success));
        map.insert("job_outcomes".to_string(), Value::Array(job_outcomes));
    }

    out
}

fn canonical_job_status(status: Option<&str>) -> Value {
    let mapped = match status.unwrap_or("") {
        "queued" | "running" => "running",
        "waiting_external" => "waiting_external",
        "succeeded" | "completed" => "completed",
        "canceled" | "failed" => "failed",
        other if !other.is_empty() => other,
        _ => "unknown",
    };
    Value::String(mapped.to_string())
}
