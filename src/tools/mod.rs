//! MCP tool wiring. Each tool is a thin wrapper around a query function.

use crate::registry::SessionRegistry;
use rmcp::{
    ServerHandler,
    model::{Implementation, ServerCapabilities, ServerInfo},
};
use std::sync::Arc;

pub mod drill_down;
pub mod lifecycle;
pub mod query;

/// The MCP server handler for pollard.
#[derive(Clone)]
pub struct PollardServer {
    // Used by tool implementations in tasks 27-29.
    #[allow(dead_code)]
    pub registry: Arc<SessionRegistry>,
}

impl PollardServer {
    pub fn new(capacity: usize) -> Self {
        Self {
            registry: Arc::new(SessionRegistry::new(capacity)),
        }
    }
}

impl ServerHandler for PollardServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
    }
}
