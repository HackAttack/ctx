//! Content-free client analytics, local usage, capability, and MCP observations.

pub mod analytics;
pub mod execution_capabilities;
#[cfg(feature = "local-usage")]
pub mod local_usage;
pub mod mcp_observation;
pub mod operation_descriptor;
