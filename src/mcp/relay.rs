//! A stable stdio MCP endpoint. Each call connects to the running app anew,
//! so startup order and app restarts never change the advertised tools.

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ClientCapabilities, ClientConfig,
    ContentBlock, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};

const UNAVAILABLE: &str = "Hylki cannot be reached. Start Hylki and turn on Settings → MCP, then retry this tool. No MCP reconnect is needed.";

pub(crate) fn run() -> ! {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hylki=warn".into()),
        )
        .init();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("MCP runtime");
    let code = runtime.block_on(async {
        match Relay.serve(rmcp::transport::stdio()).await {
            Ok(service) => {
                let _ = service.waiting().await;
                0
            }
            Err(e) => {
                tracing::error!("mcp: could not start: {e}");
                1
            }
        }
    });
    std::process::exit(code);
}

#[derive(Clone)]
struct Relay;

impl ServerHandler for Relay {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(super::SERVER_NAME, crate::VERSION))
            .with_instructions(super::tools::INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, rmcp::ErrorData> {
        Ok(ListToolsResult {
            tools: super::tools::definitions(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, rmcp::ErrorData> {
        let Some(path) = super::socket_path() else {
            return Ok(unavailable().into());
        };
        let stream = match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => stream,
            Err(e) => {
                tracing::info!("mcp: cannot connect: {e}");
                return Ok(unavailable().into());
            }
        };
        // The name of the program asking, for what Settings shows it did.
        let name = context.peer.peer_info().map_or_else(
            || "unknown".to_string(),
            |info| info.client_info.name.clone(),
        );
        let hello = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(name, crate::VERSION),
        );
        let client = match hello.serve(stream).await {
            Ok(client) => client,
            Err(e) => {
                tracing::warn!("mcp: app handshake failed: {e}");
                return Ok(unavailable().into());
            }
        };
        // Never retry a call automatically: a draft may already have opened.
        let result = client.peer().call_tool(request).await;
        let _ = client.cancel().await;
        match result {
            Ok(result) => Ok(result.into()),
            // The app answered, with a protocol error: an unknown tool, say.
            // It is the client's to hear, as it is: the connection was fine.
            Err(rmcp::ServiceError::McpError(e)) => Err(e),
            Err(e) => {
                tracing::warn!("mcp: app call failed: {e}");
                Ok(CallToolResult::error(vec![ContentBlock::text(
                    "The connection to Hylki ended before an answer arrived. If preparing a draft, check Hylki before retrying to avoid duplicates.",
                )])
                .into())
            }
        }
    }
}

fn unavailable() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(UNAVAILABLE)])
}
