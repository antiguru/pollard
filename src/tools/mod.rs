//! MCP tool wiring. Each tool is a thin wrapper around a query function.

use crate::registry::SessionRegistry;
use rmcp::{
    ServerHandler, tool_handler,
    model::{Implementation, ServerCapabilities, ServerInfo},
};
use std::sync::Arc;

pub mod drill_down;
pub mod lifecycle;
pub mod query;

/// The MCP server handler for pollard.
#[derive(Clone)]
pub struct PollardServer {
    pub registry: Arc<SessionRegistry>,
}

impl PollardServer {
    pub fn new(capacity: usize) -> Self {
        Self {
            registry: Arc::new(SessionRegistry::new(capacity)),
        }
    }

    /// Combined tool router for all lifecycle, query, and drill-down tools.
    pub fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::lifecycle_router() + Self::query_router() + Self::drill_down_router()
    }
}

#[tool_handler]
impl ServerHandler for PollardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
    }
}
