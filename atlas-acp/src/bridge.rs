//! One-way v1-to-latest value translation for legacy ACP peers.
use crate::{latest, v1, AcpError};
use serde_json::json;

pub fn v1_initialize_to_v2(
    request: v1::InitializeRequest,
) -> Result<latest::InitializeRequest, AcpError> {
    let info = request
        .client_info
        .unwrap_or_else(|| json!({"name":"v1-client","title":"v1-client"}));
    Ok(latest::InitializeRequest {
        protocol_version: latest::PROTOCOL_VERSION,
        info: serde_json::from_value(info).map_err(|e| AcpError::new(e.to_string()))?,
        capabilities: latest::Capabilities::default(),
    })
}
