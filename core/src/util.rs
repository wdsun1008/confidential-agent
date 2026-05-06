use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub fn rekor_payload(metadata: &Value) -> Result<Value> {
    if metadata.get("rv_list").is_some() {
        return Ok(metadata.clone());
    }

    let artifact_id = required_json_string(metadata, "artifact_id")?;
    let artifact_version = required_json_string(metadata, "artifact_version")?;
    let artifact_type = required_json_string(metadata, "artifact_type")?;
    let rekor_url = required_json_string(metadata, "rekor_url")?;
    let rekor_api_version = metadata
        .get("rekor_api_version")
        .and_then(|value| value.as_u64())
        .unwrap_or(1);

    let mut entry = json!({
        "id": artifact_id,
        "version": artifact_version,
        "type": artifact_type,
        "provenance_info": {
            "type": "slsa-intoto-statements",
            "rekor_url": rekor_url,
            "rekor_api_version": rekor_api_version,
        },
        "operation_type": "add",
    });

    if let Some(rv_name) = metadata.get("rv_name") {
        entry["rv_name"] = rv_name.clone();
    }
    if let Some(source) = metadata.get("provenance_source") {
        entry["provenance_source"] = source.clone();
    }

    Ok(json!({ "rv_list": [entry] }))
}

pub fn required_json_string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(|value| value.as_str())
        .with_context(|| format!("Rekor metadata is missing string field '{key}'"))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open '{}'", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read '{}'", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}
