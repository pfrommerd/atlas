//! Strict v1-to-v2 value translation used by transparent bridge hosts.
use crate::{v1, v2, AcpError};
use serde_json::{json, Value};

pub fn v1_initialize_to_v2(
    request: v1::InitializeRequest,
) -> Result<v2::InitializeRequest, AcpError> {
    let info = request
        .client_info
        .unwrap_or_else(|| json!({"name":"v1-client","title":"v1-client"}));
    Ok(v2::InitializeRequest {
        protocol_version: v2::PROTOCOL_VERSION,
        info: serde_json::from_value(info).map_err(|e| AcpError::new(e.to_string()))?,
        capabilities: v2::Capabilities::default(),
    })
}

pub fn v2_initialize_to_v1(response: v2::InitializeResponse) -> v1::InitializeResponse {
    v1::InitializeResponse {
        protocol_version: v1::PROTOCOL_VERSION,
        agent_capabilities: serde_json::to_value(response.capabilities)
            .unwrap_or(Value::Object(Default::default())),
        agent_info: serde_json::to_value(response.info).ok(),
    }
}

/// Rejects v2 updates that cannot be expressed safely to a v1 peer.
pub fn v2_update_to_v1(update: v2::SessionUpdate) -> Result<Value, AcpError> {
    let kind = update
        .update
        .get("sessionUpdate")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "user_message"
        | "agent_message"
        | "agent_thought"
        | "state_update"
        | "plan_update"
        | "terminal_update"
        | "terminal_output_chunk"
        | "tool_call_content_chunk" => Err(AcpError::new(format!(
            "ACP v2 update {kind} is unsupported by v1"
        ))),
        _ => {
            Ok(serde_json::json!({"sessionId": update.session_id, "sessionUpdate": update.update}))
        }
    }
}
