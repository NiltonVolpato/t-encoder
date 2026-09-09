//! `espielberg` — Model Context Protocol (MCP) server over stdio.
//!
//! Directs the T-Encoder-Pro through movie set terminology:
//! - `action { port }`: Connects to the device set.
//! - `take { filename? }`: Shoots a frame and saves it to disk.
//! - `cut {}`: Disconnects and releases the hardware port.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use espielberg::Director;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use tokio::sync::Mutex;

#[derive(Debug, serde::Deserialize, JsonSchema)]
pub struct ActionArgs {
    /// Serial port device path (e.g. "/dev/cu.usbmodem101").
    pub port: String,
}

#[derive(Debug, serde::Deserialize, JsonSchema)]
pub struct TakeArgs {
    /// Optional file path to save the screenshot to.
    /// Defaults to ".espielberg/take-<timestamp>.png".
    pub filename: Option<String>,
}

#[derive(Clone)]
pub struct EspielbergMcp {
    director: Arc<Mutex<Option<Director>>>,
    #[allow(dead_code)]
    tool_router: ToolRouter<EspielbergMcp>,
}

impl Default for EspielbergMcp {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl EspielbergMcp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            director: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Lights, camera, action! Opens connection to the T-Encoder-Pro on the given serial port."
    )]
    async fn action(
        &self,
        Parameters(ActionArgs { port }): Parameters<ActionArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut director_lock = self.director.lock().await;
        if director_lock.is_some() {
            return Err(McpError::invalid_request(
                "Director is already on set! Call 'cut' to release the current session before starting a new action.",
                None,
            ));
        }

        match Director::action(&port) {
            Ok(director) => {
                *director_lock = Some(director);
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Action! Director is on set and connected to '{port}'. Ready for take."
                ))]))
            }
            Err(err) => Err(McpError::internal_error(
                format!("Failed to open stage on '{port}': {err}"),
                None,
            )),
        }
    }

    #[tool(
        description = "Take! Captures a live frame from the device set and saves the shot to disk."
    )]
    async fn take(
        &self,
        Parameters(TakeArgs { filename }): Parameters<TakeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let mut director_lock = self.director.lock().await;
        let Some(director) = director_lock.as_mut() else {
            return Err(McpError::invalid_request(
                "The set is quiet. Call 'action { port: \"...\" }' before calling 'take'.",
                None,
            ));
        };

        match director.take() {
            Ok(shot) => {
                // Determine output filename
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();

                let png_path = match filename {
                    Some(custom) => PathBuf::from(custom),
                    None => PathBuf::from(format!(".espielberg/take-{now}.png")),
                };

                let raw_path = png_path.with_extension("raw");

                if let Some(parent) = png_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }

                if let Err(e) = shot.save_raw(&raw_path) {
                    return Err(McpError::internal_error(
                        format!("Failed to write raw framebuffer: {e}"),
                        None,
                    ));
                }

                if let Err(e) = shot.save_png(&png_path) {
                    return Err(McpError::internal_error(
                        format!("Failed to write PNG image: {e}"),
                        None,
                    ));
                }

                let png_str = png_path.to_string_lossy();
                let raw_str = raw_path.to_string_lossy();

                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Take successful! Captured {w}x{h} frame:\n- PNG: {png_str}\n- Raw: {raw_str}",
                    w = shot.width(),
                    h = shot.height(),
                ))]))
            }
            Err(err) => Err(McpError::internal_error(
                format!("Take failed: {err}"),
                None,
            )),
        }
    }

    #[tool(
        description = "Cut! Wraps up the current shoot, cleanly closes the serial connection, and releases the port."
    )]
    async fn cut(&self) -> Result<CallToolResult, McpError> {
        let mut director_lock = self.director.lock().await;
        let Some(director) = director_lock.take() else {
            return Err(McpError::invalid_request(
                "No action in progress to cut. Call 'action' first.",
                None,
            ));
        };

        match director.cut() {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "Cut! Session ended and serial port released. Ready for next action or flashing.",
            )])),
            Err(err) => Err(McpError::internal_error(
                format!("Failed to cleanly cut session: {err}"),
                None,
            )),
        }
    }
}

#[tool_handler]
impl ServerHandler for EspielbergMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("espielberg", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "espielberg directs the T-Encoder-Pro hardware set. Tools: 'action' opens the serial connection, 'take' captures a screenshot to disk, 'cut' cleanly closes the connection.",
            )
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting espielberg MCP server on stdio");

    let server = EspielbergMcp::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
