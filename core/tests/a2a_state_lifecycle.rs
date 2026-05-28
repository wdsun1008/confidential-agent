use confidential_agent_core::a2a::{A2aBundle, A2aBundlePeer, A2aStateFile, A2aStatePeer};
use confidential_agent_core::schema::*;
use std::collections::BTreeMap;
use std::fs;

fn state_peer(alias: &str, url: &str, scoped_services: Vec<String>) -> A2aStatePeer {
    A2aStatePeer {
        alias: Some(alias.to_string()),
        url: url.to_string(),
        scoped_services,
        signer: None,
        added_at: "2026-05-20T10:00:00Z".to_string(),
        cli_preview: None,
        cli_preview_error: None,
    }
}

#[test]
fn a2a_state_file_add_peer_and_serialize() {
    let mut state = A2aStateFile {
        version: 2,
        peers: Vec::new(),
    };

    state.peers.push(state_peer(
        "beta",
        "http://5.6.7.8:8089/.well-known/agent-card.json",
        vec![],
    ));
    state.validate().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a2a.json");
    let serialized = serde_json::to_string_pretty(&state).unwrap();
    fs::write(&path, serialized).unwrap();
    let deserialized = A2aStateFile::from_path(&path).unwrap();

    assert_eq!(deserialized.peers.len(), 1);
    assert_eq!(deserialized.peers[0].alias.as_deref(), Some("beta"));
    assert!(deserialized.peers[0].url.contains("5.6.7.8"));
}

#[test]
fn a2a_bundle_round_trip() {
    let bundle = A2aBundle {
        version: 2,
        peers: vec![A2aBundlePeer {
            alias: Some("beta".to_string()),
            url: "http://5.6.7.8:8089/.well-known/agent-card.json".to_string(),
            scoped_services: vec!["openclaw".to_string()],
            signer: None,
            fingerprint: "sha256:abcdef1234567890".to_string(),
        }],
    };

    let serialized = serde_json::to_string_pretty(&bundle).unwrap();
    let deserialized: A2aBundle = serde_json::from_str(&serialized).unwrap();

    deserialized.validate().unwrap();
    assert_eq!(deserialized.version, 2);
    assert_eq!(deserialized.peers.len(), 1);
    let peer = &deserialized.peers[0];
    assert_eq!(peer.alias.as_deref(), Some("beta"));
    assert_eq!(peer.scoped_services, vec!["openclaw"]);
    assert_eq!(peer.fingerprint, "sha256:abcdef1234567890");
}

#[test]
fn a2a_state_with_multiple_peers() {
    let state = A2aStateFile {
        version: 2,
        peers: vec![
            state_peer(
                "alpha",
                "http://1.2.3.4:8089/.well-known/agent-card.json",
                vec![],
            ),
            state_peer(
                "beta",
                "http://5.6.7.8:8089/.well-known/agent-card.json",
                vec!["openclaw".to_string()],
            ),
        ],
    };

    state.validate().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a2a.json");
    fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
    let parsed = A2aStateFile::from_path(&path).unwrap();
    assert_eq!(parsed.peers.len(), 2);
    assert_eq!(parsed.peers[0].alias.as_deref(), Some("alpha"));
    assert_eq!(parsed.peers[1].scoped_services, vec!["openclaw"]);
}

#[test]
fn daemon_status_with_a2a_peers() {
    let status = DaemonStatus {
        schema: DAEMON_STATUS_SCHEMA_VERSION.to_string(),
        service_id: "openclaw".to_string(),
        phase: "active".to_string(),
        bootstrap_generation: 1,
        mesh_generation: 2,
        applied_resources: BTreeMap::new(),
        mesh_fingerprint: Some("abc123".to_string()),
        app_ready: true,
        mesh_ready: true,
        debug_ssh_ready: false,
        a2a_peers: BTreeMap::from([(
            "beta".to_string(),
            DaemonA2aPeerStatus {
                url: "http://5.6.7.8:8089/.well-known/agent-card.json".to_string(),
                id: Some("beta-agent".to_string()),
                state: "active".to_string(),
                last_fetch_unix: Some(1716000000),
                last_success_unix: Some(1716000000),
                error: None,
                ports: vec![18789],
            },
        )]),
        last_error: None,
    };

    let json_str = serde_json::to_string_pretty(&status).unwrap();
    let parsed: DaemonStatus = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.a2a_peers.len(), 1);
    assert_eq!(parsed.a2a_peers["beta"].state, "active");
    assert_eq!(parsed.a2a_peers["beta"].ports, vec![18789]);
}

#[test]
fn a2a_state_empty_roundtrip() {
    let state = A2aStateFile::empty();
    assert_eq!(state.version, 2);
    assert!(state.peers.is_empty());

    let json_str = serde_json::to_string(&state).unwrap();
    let parsed: A2aStateFile = serde_json::from_str(&json_str).unwrap();
    assert!(parsed.peers.is_empty());
}
