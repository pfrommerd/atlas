//! Differential wire tests against the official ACP Rust SDK crates.

use agent_client_protocol::JsonRpcMessage;
use agent_client_protocol_schema::{v1, v2};
use atlas_acp::{v2 as atlas_v2, InitializeRequest, InitializeResponse};
use serde_json::json;

fn atlas_v2_initialize() -> InitializeRequest {
    InitializeRequest {
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
    let _: InitializeRequest = serde_json::from_value(reference_wire).unwrap();
    assert_eq!(wire["protocolVersion"], json!(2));
    assert_eq!(wire["info"]["name"], json!("atlas"));
}

#[test]
fn v1_initialize_response_is_accepted_by_the_shared_initializer() {
    let wire = json!({
        "protocolVersion": 1,
        "agentCapabilities": {},
        "agentInfo": {"name":"atlas", "title":"Atlas"},
    });
    let _: v1::InitializeResponse = serde_json::from_value(wire.clone()).unwrap();
    let response: InitializeResponse = serde_json::from_value(wire).unwrap();
    assert_eq!(response.protocol_version, 1);
    assert_eq!(response.info.unwrap().name, "atlas");
}

#[test]
fn v1_initialize_response_allows_agent_info_without_a_title() {
    let wire = json!({
        "protocolVersion": 1,
        "agentCapabilities": {},
        "agentInfo": {"name":"OpenCode", "version":"1.18.10"},
    });
    let response: InitializeResponse = serde_json::from_value(wire).unwrap();
    let info = response.info.unwrap();
    assert_eq!(info.name, "OpenCode");
    assert!(info.title.is_empty());
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
    let prompt = json!({
        "sessionId": "s1",
        "prompt": [{"type":"text", "text":"hi"}],
    });
    let cancel = json!({"sessionId": "s1"});
    let _: v2::PromptRequest = serde_json::from_value(prompt).unwrap();
    let _: v2::CancelSessionNotification = serde_json::from_value(cancel).unwrap();
}
