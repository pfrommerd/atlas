//! Agent Client Protocol bindings implemented on [`atlas_rpc`].
//!
//! The v2 surface intentionally covers ACP's session core. v1 remains available
//! for interoperating with existing agents through [`bridge`].

pub mod bridge;
pub mod v1;
pub mod v2;

/// The current ACP API version.
pub use v2 as latest;
/// ACP v2 is the default public surface. Use [`v1`] for explicit legacy support.
pub use v2::*;

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}
impl AcpError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            data: None,
        }
    }
}
impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
