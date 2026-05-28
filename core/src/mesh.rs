use crate::schema::{LocalServiceState, MeshBundle, MeshService, MESH_SCHEMA_VERSION};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn render_mesh_bundle(
    services: &[LocalServiceState],
    sample_reference_values: &BTreeMap<String, Value>,
    rekor_reference_values: &BTreeMap<String, Value>,
    generation: u64,
) -> MeshBundle {
    render_mesh_bundle_at(
        services,
        sample_reference_values,
        rekor_reference_values,
        generation,
        unix_timestamp(),
    )
}

pub fn render_mesh_bundle_at(
    services: &[LocalServiceState],
    sample_reference_values: &BTreeMap<String, Value>,
    rekor_reference_values: &BTreeMap<String, Value>,
    generation: u64,
    updated_at: u64,
) -> MeshBundle {
    let mut service_map = BTreeMap::new();
    for service in services {
        if service.phase != "active" {
            continue;
        }
        service_map.insert(
            service.service_id.clone(),
            MeshService {
                phase: service.phase.clone(),
                private_ip: service.deploy.private_ip.clone(),
                public_ip: service.deploy.public_ip.clone(),
                ports: service.service.ports.clone(),
                connect: service.service.connect.clone(),
            },
        );
    }

    MeshBundle {
        schema: MESH_SCHEMA_VERSION.to_string(),
        generation,
        updated_at,
        reference_values: sample_reference_values
            .iter()
            .filter(|(service_id, _)| service_map.contains_key(*service_id))
            .map(|(service_id, value)| (service_id.clone(), value.clone()))
            .collect(),
        rekor_reference_values: rekor_reference_values
            .iter()
            .filter(|(service_id, _)| service_map.contains_key(*service_id))
            .map(|(service_id, value)| (service_id.clone(), value.clone()))
            .collect(),
        services: service_map,
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
