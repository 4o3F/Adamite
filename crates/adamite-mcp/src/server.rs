use crate::{
    McpError, SnapshotOptions, SnapshotStore,
    parameters::{Format, ListDirParams, SnapshotParams},
    response::Response,
};
use adamite_access::report::{InfoReport, LsReport};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    service::ServerInitializeError,
    tool, tool_handler, tool_router,
};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const INSTRUCTIONS: &str = "Start with snapshots. If no snapshot exists or it is stale, tell the user a Windows UAC prompt will appear and ask for consent before scan_volume. Poll scan_status until done. Use snapshot_info, get_tree at /, then list_dir, top_files, ext_breakdown or search to inspect usage. Call node_info before recommending candidates, and present their size and age for the user to decide. Sizes describe on-disk allocation. Never delete, move or run cleanup commands. Elevate only through scan_volume after consent. Never retry a declined UAC prompt on your own; ask the user whether to retry.";

/// Snapshot sources, cache capacity and the age at which a snapshot becomes stale.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Discovery sources and the retained output count.
    pub snapshots: SnapshotOptions,
    /// Staleness threshold, defaulting to 24 hours.
    pub stale_after: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            snapshots: SnapshotOptions::default(),
            stale_after: Duration::from_secs(86_400),
        }
    }
}

/// Read-only snapshot tools served through rmcp with at most two blocking queries.
#[derive(Debug, Clone)]
pub struct McpServer {
    store: Arc<SnapshotStore>,
    stale_after: Duration,
    query_permits: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// Initializes discovery and the shared cache without decoding snapshots.
    ///
    /// # Errors
    /// Returns an error if snapshot source paths cannot be initialized.
    pub fn new(options: ServerOptions) -> Result<Self, McpError> {
        Ok(Self {
            store: Arc::new(SnapshotStore::new(options.snapshots)?),
            stale_after: options.stale_after,
            query_permits: Arc::new(Semaphore::new(2)),
            tool_router: Self::tool_router(),
        })
    }

    /// Serves protocol messages on standard input/output using a current-thread runtime.
    ///
    /// EOF ends request acceptance. Runtime shutdown waits for all started blocking work,
    /// including work whose response handler was cancelled during rmcp's EOF drain.
    ///
    /// # Errors
    /// Returns runtime, handshake or service-task failures. EOF before initialization is successful.
    ///
    /// # Panics
    /// Panics when called inside an asynchronous runtime. Embedded servers should use
    /// the [`ServerHandler`] implementation with their own transport.
    pub fn serve_stdio(self) -> Result<(), McpError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| McpError::io("", error))?;
        runtime.block_on(async move {
            let service = match self.serve(rmcp::transport::stdio()).await {
                Ok(service) => service,
                Err(ServerInitializeError::ConnectionClosed(_)) => return Ok(()),
                Err(error) => return Err(McpError::io("", io::Error::other(error))),
            };
            service
                .waiting()
                .await
                .map_err(|error| McpError::io("", io::Error::other(error)))?;
            Ok(())
        })
    }

    async fn query(
        &self,
        format: Format,
        work: impl FnOnce(&SnapshotStore, Duration) -> Result<Response, McpError> + Send + 'static,
    ) -> CallToolResult {
        let result = async {
            let permit = Arc::clone(&self.query_permits)
                .acquire_owned()
                .await
                .map_err(|error| McpError::io("", io::Error::other(error)))?;
            let store = Arc::clone(&self.store);
            let stale_after = self.stale_after;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                work(&store, stale_after)
            })
            .await
            .map_err(|error| McpError::io("", io::Error::other(error)))?
        }
        .await;
        match result {
            Ok(response) => response.finish(format),
            Err(error) => Response::error(error, format),
        }
    }
}

#[tool_router]
impl McpServer {
    /// Lists discovered snapshot metadata, including scan times for cached snapshots.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn snapshots(&self, Parameters(params): Parameters<SnapshotParams>) -> CallToolResult {
        self.query(params.format, move |store, stale_after| {
            let mut entries = store.snapshots()?;
            if let Some(selection) = params.snapshot {
                let path = store.select(Some(&selection))?;
                entries.retain(|entry| entry.path.as_ref() == Some(&path));
            }
            Response::catalog(entries, stale_after)
        })
        .await
    }

    /// Shows volume metadata, tree totals and scan diagnostics for a saved snapshot.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn snapshot_info(
        &self,
        Parameters(params): Parameters<SnapshotParams>,
    ) -> CallToolResult {
        self.query(params.format, move |store, stale_after| {
            let path = store.select(params.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let report = InfoReport::new("snapshot_info", &path, &output)
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            Response::report(&report, &report.envelope, stale_after, |writer| {
                report.write_details(writer, false)
            })
        })
        .await
    }

    /// Lists a directory's children in allocation order, with offset-based pagination.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn list_dir(&self, Parameters(params): Parameters<ListDirParams>) -> CallToolResult {
        self.query(params.format, move |store, stale_after| {
            let limit = usize::try_from(params.limit)
                .ok()
                .filter(|limit| (1..=500).contains(limit))
                .ok_or_else(|| McpError::InvalidArgument {
                    message: format!("limit {} is outside the allowed range", params.limit),
                    allowed: "Pass limit from 1 through 500.".into(),
                })?;
            let path = store.select(params.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let mut report = LsReport::new(&path, &output, &params.path, params.offset, limit)
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            report.envelope.command = "list_dir";
            let mut response =
                Response::report(&report, &report.envelope, stale_after, |writer| {
                    report.write_entries(writer, false)
                })?;
            response.page(report.offset, report.items.len(), report.total_children);
            Ok(response)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResponse, rmcp::ErrorData> {
        let route = self
            .tool_router
            .map
            .get(request.name.as_ref())
            .ok_or_else(|| rmcp::ErrorData::invalid_params("tool not found", None))?;
        // Invoke the route directly so deserialization failures remain protocol errors.
        // ToolRouter::call converts those failures to tool results in rmcp 3.9.
        let context = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        (route.call)(context).await
    }

    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("adamite", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}
