//! Product-neutral application orchestration for coding-agent integrations.

mod integration;

pub mod integrations;
pub mod mcp;
pub mod mcp_tool_call;
pub mod skill;
pub mod tool_backend;

pub use integration::{IntegrationResultFact, IntegrationTelemetryFacts, TargetSelectionFact};
pub use mcp::ProductIdentity;
