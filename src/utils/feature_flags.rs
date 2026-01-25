pub fn is_truthy(value: impl AsRef<str>) -> bool {
    matches!(
        value.as_ref().trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn is_truthy_any_env(keys: &[&str]) -> bool {
    keys.iter()
        .any(|key| std::env::var(key).ok().map(is_truthy).unwrap_or(false))
}

pub fn is_unsafe_local_enabled() -> bool {
    is_truthy_any_env(&["INFRA_UNSAFE_LOCAL"])
}

pub fn is_allow_secret_export_enabled() -> bool {
    is_truthy_any_env(&["INFRA_ALLOW_SECRET_EXPORT"])
}
