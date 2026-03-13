pub const BUNDLED_RUNBOOKS_MANIFEST_URI: &str = "bundled://runbooks.json";
pub const BUNDLED_CAPABILITIES_MANIFEST_URI: &str = "bundled://capabilities.json";

pub fn bundled_runbooks_json() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/runbooks.json"))
}

pub fn bundled_capabilities_json() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/capabilities.json"))
}
