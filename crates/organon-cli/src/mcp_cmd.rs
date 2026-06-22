//! `organon mcp` — starts the MCP server (stdio or HTTP).

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use log::info;
use organon_core::config::OrgConfig;

/// Start the MCP server.
///
/// In stdio mode (default) the process speaks the MCP protocol on stdin/stdout — suitable
/// for Claude Desktop and other MCP clients that launch a subprocess.
/// In SSE mode (`--sse`) an HTTP server is bound at `config.server.host:port`.
pub(crate) fn cmd_mcp_with_config(
    sse: bool,
    path: Option<&Path>,
    scope: Option<&Path>,
    global: bool,
    config: &OrgConfig,
) -> Result<()> {
    if global && (path.is_some() || scope.is_some()) {
        bail!("use either --global or a scope path, not both");
    }
    let session_scope = if global {
        None
    } else {
        Some(canonical_scope(
            scope.or(path).unwrap_or_else(|| Path::new(".")),
        ))
    };
    info!("starting MCP server (sse={sse}) scope={session_scope:?}");
    let _ = env_logger::try_init();
    let runtime = tokio::runtime::Runtime::new()?;
    let service = organon_mcp::McpService::from_config_with_scope(config.clone(), session_scope);
    if sse {
        let host = config.server.host.clone();
        let port = config.server.port;
        return runtime.block_on(organon_mcp::serve_streamable_http(service, host, port));
    }
    runtime.block_on(organon_mcp::serve_stdio(service))
}

/// Resolve a path to its canonical form, falling back to the original on error.
pub(crate) fn canonical_scope(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
