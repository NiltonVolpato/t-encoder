//! `espielberg` — Model Context Protocol (MCP) server over stdio.
//!
//! Directs the T-Encoder-Pro through movie set terminology:
//! - `action { port }`: Connects to the device set.
//! - `take { filename? }`: Shoots a frame and saves it to disk.
//! - `cut {}`: Disconnects and releases the hardware port.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use espielberg::{Director, SwipeDirection};
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
    /// Defaults to ".espielberg/take-<timestamp>.png" in the working directory.
    pub filename: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub struct RotateCueArgs {
    /// Number of detents to rotate (+1 clockwise, -1 counter-clockwise).
    pub delta: i32,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub struct TapCueArgs {
    /// X coordinate on the 390x390 screen (0..390).
    pub x: i32,
    /// Y coordinate on the 390x390 screen (0..390).
    pub y: i32,
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub struct SwipeCueArgs {
    /// Swipe direction ("left", "right", "up", "down").
    pub direction: SwipeDirection,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, JsonSchema)]
pub struct EmptyObject {}

#[derive(Debug, Clone, Copy, serde::Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ActionTrigger {
    Bool(bool),
    Empty(EmptyObject),
}

impl ActionTrigger {
    #[must_use]
    pub const fn is_active(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            Self::Empty(_) => true,
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize, JsonSchema)]
pub struct CueArgs {
    /// Rotate the rotary dial by delta detents (+1 CW, -1 CCW).
    pub rotate: Option<RotateCueArgs>,
    /// Short press the dial button (e.g. `true` or `{}`).
    pub press: Option<ActionTrigger>,
    /// Long press the dial button (e.g. `true` or `{}`).
    pub long_press: Option<ActionTrigger>,
    /// Tap the touch panel at (x, y) coordinates.
    pub tap: Option<TapCueArgs>,
    /// Swipe across the panel.
    pub swipe: Option<SwipeCueArgs>,
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
        description = "Take! Captures a live frame from the device set and saves the shot to disk (defaults to .espielberg/take-<timestamp>.png, returning absolute paths)."
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

                let absolute_png_path = std::fs::canonicalize(&png_path).unwrap_or_else(|_| {
                    if png_path.is_absolute() {
                        png_path
                    } else {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(&png_path))
                            .unwrap_or(png_path)
                    }
                });
                let absolute_raw_path = std::fs::canonicalize(&raw_path).unwrap_or_else(|_| {
                    if raw_path.is_absolute() {
                        raw_path
                    } else {
                        std::env::current_dir()
                            .map(|cwd| cwd.join(&raw_path))
                            .unwrap_or(raw_path)
                    }
                });

                let png_path_string = absolute_png_path.display();
                let raw_path_string = absolute_raw_path.display();

                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Take successful! Captured {w}x{h} frame:\n- PNG: {png_path_string}\n- Raw: {raw_path_string}",
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

    #[tool(
        description = "Cue! Injects an input event (dial rotation, button press, tap, or swipe) on the device set."
    )]
    async fn cue(&self, Parameters(args): Parameters<CueArgs>) -> Result<CallToolResult, McpError> {
        let mut director_lock = self.director.lock().await;
        let Some(director) = director_lock.as_mut() else {
            return Err(McpError::invalid_request(
                "The set is quiet. Call 'action { port: \"...\" }' before calling 'cue'.",
                None,
            ));
        };

        let mut cued_descriptions = Vec::new();

        if let Some(rotate) = args.rotate {
            director.rotate(rotate.delta).map_err(|e| {
                McpError::internal_error(format!("Failed to cue rotate: {e}"), None)
            })?;
            cued_descriptions.push(format!("rotate({})", rotate.delta));
        }

        if args.press.is_some_and(|p| p.is_active()) {
            director
                .press()
                .map_err(|e| McpError::internal_error(format!("Failed to cue press: {e}"), None))?;
            cued_descriptions.push("press".to_string());
        }

        if args.long_press.is_some_and(|p| p.is_active()) {
            director.long_press().map_err(|e| {
                McpError::internal_error(format!("Failed to cue long_press: {e}"), None)
            })?;
            cued_descriptions.push("long_press".to_string());
        }

        if let Some(tap) = args.tap {
            director
                .tap(tap.x, tap.y)
                .map_err(|e| McpError::internal_error(format!("Failed to cue tap: {e}"), None))?;
            cued_descriptions.push(format!("tap({}, {})", tap.x, tap.y));
        }

        if let Some(swipe) = args.swipe {
            director
                .swipe(swipe.direction)
                .map_err(|e| McpError::internal_error(format!("Failed to cue swipe: {e}"), None))?;
            cued_descriptions.push(format!("swipe({})", swipe.direction.as_str()));
        }

        if cued_descriptions.is_empty() {
            return Err(McpError::invalid_request(
                "No cue specified! Provide at least one cue: rotate, press, long_press, tap, or swipe.",
                None,
            ));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Cue delivered to set: {}",
            cued_descriptions.join(", ")
        ))]))
    }

    #[tool(
        description = "Reset! Reboots the device set back to initial boot state, waits for reboot, and reconnects."
    )]
    async fn reset(&self) -> Result<CallToolResult, McpError> {
        let mut director_lock = self.director.lock().await;
        let Some(director) = director_lock.as_mut() else {
            return Err(McpError::invalid_request(
                "The set is quiet. Call 'action { port: \"...\" }' before calling 'reset'.",
                None,
            ));
        };

        match director.reset() {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(
                "Reset successful! Device rebooted and reconnected to launcher.",
            )])),
            Err(err) => Err(McpError::internal_error(
                format!("Failed to reset device: {err}"),
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
                "espielberg directs the T-Encoder-Pro hardware set. Tools: 'action' opens the serial connection, 'cue' injects an input event, 'take' captures a screenshot to disk (saved to .espielberg/ by default, returning absolute paths), 'reset' reboots the device, 'cut' cleanly closes the connection.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::serde_json;

    #[test]
    fn test_cue_args_deserialization() {
        // Rotate
        let json = serde_json::json!({ "rotate": { "delta": -3 } });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.rotate.unwrap().delta, -3);

        // Press with boolean
        let json = serde_json::json!({ "press": true });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        assert!(args.press.unwrap().is_active());

        // Press with empty object
        let json = serde_json::json!({ "press": {} });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        assert!(args.press.unwrap().is_active());

        // Long press with boolean
        let json = serde_json::json!({ "long_press": true });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        assert!(args.long_press.unwrap().is_active());

        // Tap
        let json = serde_json::json!({ "tap": { "x": 120, "y": 240 } });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        let tap = args.tap.unwrap();
        assert_eq!(tap.x, 120);
        assert_eq!(tap.y, 240);

        // Swipe
        let json = serde_json::json!({ "swipe": { "direction": "left" } });
        let args: CueArgs = serde_json::from_value(json).unwrap();
        assert_eq!(args.swipe.unwrap().direction, SwipeDirection::Left);
    }

    #[tokio::test]
    async fn test_tool_router_has_all_movie_tools() {
        let router = EspielbergMcp::tool_router();
        assert!(router.has_route("action"));
        assert!(router.has_route("cue"));
        assert!(router.has_route("take"));
        assert!(router.has_route("reset"));
        assert!(router.has_route("cut"));
    }
}
