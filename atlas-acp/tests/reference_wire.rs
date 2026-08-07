//! Differential wire tests against the official ACP Rust SDK crates.

use agent_client_protocol::JsonRpcMessage;
use agent_client_protocol_schema::{v1, v2};
use atlas_acp::{bridge, v1 as atlas_v1, v2 as atlas_v2};
use serde_json::json;

fn atlas_v2_initialize() -> atlas_v2::InitializeRequest {
    atlas_v2::InitializeRequest {
        protocol_version: 2,
        info: atlas_v2::Implementation {
            name: "atlas".into(),
            title: "Atlas".into(),
            version: Some("0.1".into()),
        },
        capabilities: atlas_v2::Capabilities::default(),
    }
}

#[test]
fn v2_initialize_round_trips_through_the_reference_schema() {
    let atlas = atlas_v2_initialize();
    let wire = serde_json::to_value(&atlas).unwrap();
    let reference: v2::InitializeRequest = serde_json::from_value(wire.clone()).unwrap();
    let reference_wire = serde_json::to_value(reference).unwrap();
    let _: atlas_v2::InitializeRequest = serde_json::from_value(reference_wire).unwrap();
    assert_eq!(wire["protocolVersion"], json!(2));
    assert_eq!(wire["info"]["name"], json!("atlas"));
}

#[test]
fn v1_initialize_round_trips_through_the_reference_schema() {
    let atlas = atlas_v1::InitializeRequest {
        protocol_version: 1,
        client_capabilities: json!({}),
        client_info: Some(json!({"name":"atlas"})),
    };
    let wire = serde_json::to_value(&atlas).unwrap();
    let reference: v1::InitializeRequest = serde_json::from_value(wire.clone()).unwrap();
    let reference_wire = serde_json::to_value(reference).unwrap();
    let _: atlas_v1::InitializeRequest = serde_json::from_value(reference_wire).unwrap();
    assert_eq!(wire["protocolVersion"], json!(1));
}

#[test]
fn official_high_level_sdk_recognizes_atlas_initialize_method() {
    let params = serde_json::to_value(atlas_v2_initialize()).unwrap();
    assert!(
        <agent_client_protocol::schema::v2::InitializeRequest as JsonRpcMessage>::matches_method(
            "initialize"
        )
    );
    let parsed =
        <agent_client_protocol::schema::v2::InitializeRequest as JsonRpcMessage>::parse_message(
            "initialize",
            &params,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["protocolVersion"],
        json!(2)
    );
}

#[test]
fn atlas_v2_prompt_and_cancel_use_reference_wire_names() {
    let prompt = atlas_v2::PromptRequest {
        session_id: "s1".into(),
        prompt: vec![json!({"type":"text", "text":"hi"})],
    };
    let cancel = atlas_v2::SessionRequest {
        session_id: "s1".into(),
    };
    let prompt_wire = serde_json::to_value(prompt).unwrap();
    let cancel_wire = serde_json::to_value(cancel).unwrap();
    let _: v2::PromptRequest = serde_json::from_value(prompt_wire).unwrap();
    let _: v2::CancelSessionNotification = serde_json::from_value(cancel_wire).unwrap();
}

#[test]
fn bridge_adapts_v1_initialization_to_latest() {
    let latest = bridge::v1_initialize_to_v2(atlas_v1::InitializeRequest {
        protocol_version: 1,
        client_capabilities: json!({}),
        client_info: Some(json!({"name":"legacy", "title":"Legacy"})),
    })
    .unwrap();
    assert_eq!(latest.protocol_version, atlas_v2::PROTOCOL_VERSION);
    assert_eq!(latest.info.name, "legacy");
}
