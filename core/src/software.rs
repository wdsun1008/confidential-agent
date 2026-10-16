//! Exact build defaults shared with the installers and E2E runner.
pub const VERSIONS_JSON: &str = include_str!("../../software-versions.json");

pub fn version(component: &str) -> String {
    serde_json::from_str::<serde_json::Value>(VERSIONS_JSON)
        .expect("checked-in software versions must be JSON")
        .get(component)
        .and_then(|v| v.as_str())
        .expect("component must have an exact checked-in version")
        .to_owned()
}
