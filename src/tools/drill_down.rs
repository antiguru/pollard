//! Drill-down MCP tools: source_for_function, asm_for_function.

use crate::error::ToolError;
use crate::query::{address_to_function, asm, source};
use crate::tools::PollardServer;
use rmcp::ErrorData;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{tool, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;

async fn session(
    server: &PollardServer,
    profile_id: &str,
) -> Result<std::sync::Arc<crate::session::ProfileSession>, ToolError> {
    server.registry.get_or_error(profile_id).await
}

#[derive(Deserialize, JsonSchema)]
pub struct SourceForFunctionArgs {
    pub profile_id: String,
    pub function: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub with_samples: Option<bool>,
    #[serde(default)]
    pub whole_file: Option<bool>,
    /// When true, the function matcher also considers DWARF inline frames
    /// — letting callers ask for the source of an inlined function (e.g.
    /// `core::iter::Sum::sum`) instead of only the enclosing native one.
    #[serde(default)]
    pub expand_inlines: Option<bool>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AddressToFunctionArgs {
    pub profile_id: String,
    /// Library-relative address to resolve. Accepts a JSON number; for hex,
    /// callers should pre-convert. Must fit in u32.
    pub address: u64,
    /// Optional substring matched against `lib.name`, `lib.debug_name`,
    /// `lib.path`, or `lib.debug_path`. Case-sensitive. When omitted,
    /// every loaded library is tried in order until one resolves.
    #[serde(default)]
    pub module: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct AsmForFunctionArgs {
    pub profile_id: String,
    pub function: String,
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub with_samples: Option<bool>,
}

#[tool_router(router = drill_down_router, vis = "pub(crate)")]
impl PollardServer {
    #[tool(
        name = "source_for_function",
        description = "Source listing with per-line sample counts."
    )]
    pub async fn source_for_function(
        &self,
        Parameters(args): Parameters<SourceForFunctionArgs>,
    ) -> Result<Json<source::SourceListing>, ErrorData> {
        let session = session(self, &args.profile_id).await?;
        let q_args = source::Args {
            function: args.function,
            module: args.module,
            with_samples: args.with_samples.unwrap_or(true),
            whole_file: args.whole_file.unwrap_or(false),
            expand_inlines: args.expand_inlines.unwrap_or(false),
        };
        let result = source::source_for_function(session.profile(), &q_args).await?;
        Ok(Json(result))
    }

    #[tool(
        name = "address_to_function",
        description = "Resolve a single library-relative address to a function name (and file/line where available). Diagnostic for profiles with unresolved hex offsets — wraps the same wholesym lookup pollard runs on load."
    )]
    pub async fn address_to_function(
        &self,
        Parameters(args): Parameters<AddressToFunctionArgs>,
    ) -> Result<Json<address_to_function::Output>, ErrorData> {
        let session = session(self, &args.profile_id).await?;
        let q_args = address_to_function::Args {
            address: args.address,
            module: args.module,
        };
        let result = address_to_function::address_to_function(session.profile(), &q_args).await?;
        Ok(Json(result))
    }

    #[tool(
        name = "asm_for_function",
        description = "Disassembly with per-instruction sample counts."
    )]
    pub async fn asm_for_function(
        &self,
        Parameters(args): Parameters<AsmForFunctionArgs>,
    ) -> Result<Json<asm::AsmListing>, ErrorData> {
        let session = session(self, &args.profile_id).await?;
        let q_args = asm::Args {
            function: args.function,
            module: args.module,
            with_samples: args.with_samples.unwrap_or(true),
        };
        let result = asm::asm_for_function(session.profile(), &q_args).await?;
        Ok(Json(result))
    }
}
