//! MCP tool wiring. Each tool is a thin wrapper around a query function.

use crate::registry::SessionRegistry;
use rmcp::{
    ErrorData, RoleServer, ServerHandler,
    model::{
        CacheScope, Implementation, ListResourcesResult, PaginatedRequestParams,
        ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, ServerCapabilities,
        ServerConfig,
    },
    service::RequestContext,
    tool_handler,
};
use std::sync::Arc;

pub mod budget;
pub mod drill_down;
pub mod guides;
pub mod lifecycle;
pub mod query;
pub mod views;

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

    /// Combined tool router for all lifecycle, query, drill-down, and view tools.
    pub fn tool_router() -> rmcp::handler::server::router::tool::ToolRouter<Self> {
        Self::lifecycle_router()
            + Self::query_router()
            + Self::drill_down_router()
            + Self::views_router()
    }
}

#[tool_handler]
impl ServerHandler for PollardServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(guides::INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        // Protocol 2026-07-28 requires ttlMs and cacheScope on results, and
        // rmcp leaves them unset. Clients reject results without them.
        Ok(ListResourcesResult::with_all_items(guides::list())
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Public))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        match guides::read(&request.uri) {
            Some(contents) => Ok(ReadResourceResult::new(vec![contents])
                .with_ttl_ms(0)
                .with_cache_scope(CacheScope::Public)
                .into()),
            None => Err(ErrorData::resource_not_found(
                format!("unknown resource: {}", request.uri),
                None,
            )),
        }
    }
}
