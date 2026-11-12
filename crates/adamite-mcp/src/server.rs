use crate::{
    McpError, SnapshotOptions, SnapshotStore,
    parameters::{
        BreakdownParams, Format, GroupBy, ListDirParams, NodeParams, ScanParams, ScanStatusParams,
        SearchParams, SnapshotParams, TopParams, TreeParams,
    },
    response::Response,
    scan::ScanJobs,
};
use adamite_access::{
    AllocationShare, BreakdownBy, BreakdownStats, DirectoryLookup, ScanOptions,
    report::{
        ExtReport, InfoReport, KindReport, LsReport, NodeReport, SearchReport, TopReport,
        TreeReport,
    },
};
use adamite_core::snapshot::{FileFilter, FileKind, NamePattern};
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    service::ServerInitializeError,
    tool, tool_handler, tool_router,
};
use std::{io, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

const INSTRUCTIONS: &str = "Start with snapshots. If no snapshot exists or it is stale, ask for consent before scan_volume; for a Windows drive, explain that a UAC prompt may appear. Poll scan_status until done. Use snapshot_info, get_tree at /, then list_dir, top_files, ext_breakdown or search to inspect usage. Call node_info before recommending candidates, and present their size and age for the user to decide. Sizes describe on-disk allocation. Never delete, move or run cleanup commands. Elevate only through scan_volume after consent. Never retry a declined UAC prompt on your own; ask the user whether to retry.";

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

/// Scan and snapshot tools served through rmcp with at most two blocking queries.
#[derive(Debug, Clone)]
pub struct McpServer {
    store: Arc<SnapshotStore>,
    stale_after: Duration,
    query_permits: Arc<Semaphore>,
    tool_router: ToolRouter<Self>,
    scans: Arc<ScanJobs>,
    launch_permit: Arc<Semaphore>,
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
            scans: Arc::default(),
            launch_permit: Arc::new(Semaphore::new(1)),
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
    /// Starts one scan and replaces its saved snapshot after successful completion. Request user consent before calling; never retry declined elevation without asking.
    #[tool(annotations(
        read_only_hint = false,
        destructive_hint = false,
        idempotent_hint = false,
        open_world_hint = false
    ))]
    async fn scan_volume(&self, Parameters(params): Parameters<ScanParams>) -> CallToolResult {
        let Ok(launch) = Arc::clone(&self.launch_permit).try_acquire_owned() else {
            return Response::error(McpError::ScanBusy, params.format);
        };
        let scans = Arc::clone(&self.scans);
        self.query(params.format, move |store, _| {
            let _launch = launch;
            scans.start(
                &params.volume,
                ScanOptions {
                    profile: Some(params.profile.into()),
                    ..ScanOptions::default()
                },
                store.directory(),
            )
        })
        .await
    }

    /// Reports the current or last scan's progress and retained outcome. A volume mismatch is invalid_argument.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn scan_status(
        &self,
        Parameters(params): Parameters<ScanStatusParams>,
    ) -> CallToolResult {
        let scans = Arc::clone(&self.scans);
        self.query(params.format, move |_, _| {
            scans.status(params.volume.as_deref())
        })
        .await
    }

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
            let limit = Response::range("limit", params.limit, 500)?;
            let path = store.select(params.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let mut report = LsReport::new(&path, &output, &params.path, params.offset, limit)
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            report.envelope.command = "list_dir";
            let mut response =
                Response::report(&report, &report.envelope, stale_after, |writer| {
                    report.write_entries(writer, false)
                })?;
            response.page(report.offset, report.total_children);
            Ok(response)
        })
        .await
    }

    /// Summarizes a directory within a row budget, collapsing smaller sibling groups.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn get_tree(&self, Parameters(params): Parameters<TreeParams>) -> CallToolResult {
        self.query(params.common.format, move |store, stale_after| {
            let budget = Response::range("budget", params.budget, 400)?;
            let share =
                AllocationShare::from_percent(params.min_share_percent).ok_or_else(|| {
                    McpError::InvalidArgument {
                        message: "invalid min_share_percent".into(),
                        allowed: "Pass a finite percentage from 0 through 100.".into(),
                    }
                })?;
            let path = store.select(params.common.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let mut report =
                TreeReport::new(&path, &output, &params.path, budget, share.basis_points())
                    .map_err(|error| Response::report_error(error, &output.snapshot))?;
            report.envelope.command = "get_tree";
            Response::report(&report, &report.envelope, stale_after, |writer| {
                report.write_entries(writer, false)
            })
        })
        .await
    }

    /// Lists the largest primary files, optionally filtered by directory, extensions or kind.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn top_files(&self, Parameters(params): Parameters<TopParams>) -> CallToolResult {
        self.query(params.common.format, move |store, stale_after| {
            let n = Response::range("n", params.n, 100)?;
            if params.ext.is_some() && params.kind.is_some() {
                return Err(McpError::InvalidArgument {
                    message: "ext and kind are mutually exclusive".into(),
                    allowed: "Pass either ext or kind, or omit both.".into(),
                });
            }
            let filter = match (params.ext, params.kind) {
                (Some(ext), None) => {
                    Some(FileFilter::extensions(ext).map_err(Response::query_error)?)
                }
                (None, Some(kind)) => Some(FileFilter::Kind(
                    kind.parse::<FileKind>().map_err(Response::query_error)?,
                )),
                _ => None,
            };
            let path = store.select(params.common.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let mut report = TopReport::select(
                &path,
                &output,
                u32::try_from(n).map_err(|error| McpError::io("", io::Error::other(error)))?,
                params.under.as_deref(),
                filter.as_ref(),
            )
            .map_err(|error| Response::report_error(error, &output.snapshot))?;
            report.envelope.command = "top_files";
            Response::report(&report, &report.envelope, stale_after, |writer| {
                report.write_entries(writer, false)
            })
        })
        .await
    }

    /// Groups primary file allocation by extension or file category.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn ext_breakdown(
        &self,
        Parameters(params): Parameters<BreakdownParams>,
    ) -> CallToolResult {
        self.query(params.common.format, move |store, stale_after| {
            let n = Response::range("n", params.n, 200)? as u64;
            let path = store.select(params.common.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let by = match params.by {
                GroupBy::Extension => BreakdownBy::Extension,
                GroupBy::Kind => BreakdownBy::Kind,
            };
            match BreakdownStats::new(&output.snapshot, params.under.as_deref(), by)
                .map_err(|error| Response::report_error(error, &output.snapshot))?
            {
                BreakdownStats::Extensions(stats) => {
                    let mut report = ExtReport::from_stats(&path, &output, &stats, n);
                    report.envelope.command = "ext_breakdown";
                    Response::report(&report, &report.envelope, stale_after, |writer| {
                        report.write_entries(writer, false)
                    })
                }
                BreakdownStats::Kinds(stats) => {
                    let mut report = KindReport::new(&path, &output, &stats, n);
                    report.envelope.command = "ext_breakdown";
                    Response::report(&report, &report.envelope, stale_after, |writer| {
                        report.write_entries(writer, false)
                    })
                }
            }
        })
        .await
    }

    /// Searches names beneath a directory and returns an offset-based page.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn search(&self, Parameters(params): Parameters<SearchParams>) -> CallToolResult {
        self.query(params.common.format, move |store, stale_after| {
            let limit = Response::range("limit", params.limit, 500)?;
            let pattern = NamePattern::glob(&params.pattern).map_err(Response::query_error)?;
            let path = store.select(params.common.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let root = output
                .snapshot
                .resolve_directory(params.under.as_deref().unwrap_or("/"))
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            let page = output
                .snapshot
                .search(root, &pattern, params.offset, limit)
                .map_err(Response::query_error)?;
            let report = SearchReport::new(&path, &output, &page, params.offset)
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            let mut response =
                Response::report(&report, &report.envelope, stale_after, |writer| {
                    report.write_entries(writer, false)
                })?;
            response.page(
                params.offset,
                usize::try_from(report.total_matches)
                    .map_err(|error| McpError::io("", io::Error::other(error)))?,
            );
            Ok(response)
        })
        .await
    }

    /// Shows one node's identity, allocation, subtree totals, flags, hard links and timestamp.
    #[tool(annotations(read_only_hint = true, idempotent_hint = true, open_world_hint = false))]
    async fn node_info(&self, Parameters(params): Parameters<NodeParams>) -> CallToolResult {
        self.query(params.common.format, move |store, stale_after| {
            let path = store.select(params.common.snapshot.as_deref())?;
            let output = store.load_path(&path)?;
            let report = NodeReport::new(&path, &output, &params.path)
                .map_err(|error| Response::report_error(error, &output.snapshot))?;
            Response::report(&report, &report.envelope, stale_after, |writer| {
                report.write_details(writer, false)
            })
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
