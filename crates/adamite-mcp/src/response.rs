use crate::{McpError, SnapshotInfo, parameters::Format};
use adamite_access::{DirectoryLookup, ReportError, report::Envelope};
use adamite_core::snapshot::{NodeId, Snapshot};
use adamite_format::{Age, Escaped, ScanTime};
use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::Path,
    time::{Duration, SystemTime},
};

/// A complete response assembled before wire encoding; the item cap belongs here.
#[derive(Debug)]
pub(crate) struct Response {
    text: String,
    json: Value,
}

impl Response {
    pub(crate) fn report(
        report: &impl Serialize,
        envelope: &Envelope<'_>,
        stale_after: Duration,
        body: impl FnOnce(&mut Vec<u8>) -> io::Result<()>,
    ) -> Result<Self, McpError> {
        let now = SystemTime::now();
        let snapshot = &envelope.snapshot;
        let mut bytes = Vec::new();
        envelope
            .write_allocation_header(&mut bytes, now)
            .map_err(Self::output_error)?;
        body(&mut bytes).map_err(Self::output_error)?;
        let mut json = serde_json::to_value(report).map_err(Self::encoding_error)?;
        json["snapshot"]["path"] = json!(snapshot.source);
        Self::age(&mut json["snapshot"], snapshot.scanned_at, now, stale_after);
        json["truncated"] = json!(false);
        json["next_offset"] = Value::Null;
        Ok(Self {
            text: String::from_utf8(bytes)
                .map_err(|error| Self::output_error(io::Error::other(error)))?,
            json,
        })
    }

    pub(crate) fn catalog(
        entries: Vec<SnapshotInfo>,
        stale_after: Duration,
    ) -> Result<Self, McpError> {
        let now = SystemTime::now();
        let mut text = Vec::new();
        let mut items = Vec::with_capacity(entries.len());
        for entry in entries {
            let modified = entry
                .modified
                .map_or(ScanTime(None), ScanTime::from_system_time);
            let path = entry.path.as_deref().map(Path::to_string_lossy);
            let error = entry.error.as_ref().map(|error| {
                format!(
                    "error[{}]: {}\nnext: {}",
                    error.code(),
                    error,
                    error.next_step()
                )
            });
            writeln!(
                text,
                "{} · {} bytes · modified {} ({}) · scanned {} ({:#}){}",
                Escaped(path.as_deref().unwrap_or("unknown")),
                entry
                    .byte_len
                    .map_or_else(|| "unknown".into(), |size| size.to_string()),
                modified,
                Age::new(entry.modified, now),
                entry.scan_time,
                entry.scan_time.age(now),
                error
                    .as_ref()
                    .map_or_else(String::new, |error| format!(" · {}", Self::one_line(error)))
            )
            .map_err(Self::output_error)?;
            let mut item = json!({
                "name": entry.name.as_deref().map(|name| name.to_string_lossy()),
                "path": path,
                "byte_len": entry.byte_len,
                "modified": modified.rfc3339(),
                "scanned_at": entry.scan_time.rfc3339(),
                "error": error,
            });
            Self::age(&mut item, entry.scan_time, now, stale_after);
            items.push(item);
        }
        if items.is_empty() {
            writeln!(text, "No snapshots discovered.").map_err(Self::output_error)?;
        }
        Ok(Self {
            text: String::from_utf8(text)
                .map_err(|error| Self::output_error(io::Error::other(error)))?,
            json: json!({
                "schema": "adamite/1",
                "command": "snapshots",
                "snapshot": {"path": null, "age_seconds": null, "stale": null},
                "units": {"size": "bytes"},
                "items": items,
                "truncated": false,
                "next_offset": null,
            }),
        })
    }

    pub(crate) fn page(&mut self, offset: usize, count: usize, total: usize) {
        let end = offset.saturating_add(count);
        if end < total {
            self.json["next_offset"] = json!(end);
            self.text.push_str(&format!(
                "showing {}–{end} of {total} · next offset={end}\n",
                offset.saturating_add(1)
            ));
        }
    }

    pub(crate) fn finish(self, format: Format) -> CallToolResult {
        match format {
            Format::Text => CallToolResult::success(vec![ContentBlock::text(self.text)]),
            Format::Json => CallToolResult::structured(self.json),
        }
    }

    pub(crate) fn error(error: McpError, format: Format) -> CallToolResult {
        let message = format!(
            "error[{}]: {}\nnext: {}",
            error.code(),
            Self::one_line(&error.to_string()),
            Self::one_line(&error.next_step())
        );
        match format {
            Format::Text => CallToolResult::error(vec![ContentBlock::text(message)]),
            Format::Json => {
                CallToolResult::structured_error(json!({"schema": "adamite/1", "error": message}))
            }
        }
    }

    pub(crate) fn report_error(error: ReportError, snapshot: &Snapshot) -> McpError {
        match error {
            ReportError::PathNotFound { path, .. } => {
                let mut prefix = String::new();
                let mut directory = "/".to_owned();
                for component in path.split(['/', '\\']).filter(|part| !part.is_empty()) {
                    prefix.push('/');
                    prefix.push_str(component);
                    let Ok(id) = snapshot.resolve_directory(&prefix) else {
                        break;
                    };
                    if let Some(path) = Self::selectable_path(snapshot, id) {
                        directory = path;
                    }
                }
                McpError::PathNotFound { path, directory }
            }
            ReportError::AmbiguousPath {
                path, candidates, ..
            } => {
                let candidates = candidates
                    .into_iter()
                    .filter_map(|id| Self::id_path(snapshot, id))
                    .collect();
                McpError::AmbiguousPath { path, candidates }
            }
            ReportError::NotADirectory { path } => McpError::NotADirectory { path },
            ReportError::Usage { message } => McpError::InvalidArgument {
                message: message.into(),
                allowed: format!(
                    "Use offset + limit at most {}; limit must be 1 through 500.",
                    usize::MAX
                ),
            },
            source => McpError::io("", io::Error::other(source)),
        }
    }

    fn age(value: &mut Value, time: ScanTime, now: SystemTime, stale_after: Duration) {
        let age = time.age_seconds(now);
        value["age_seconds"] = json!(age);
        value["stale"] = json!(age.map(|seconds| seconds >= stale_after.as_secs()));
    }

    fn selectable_path(snapshot: &Snapshot, id: NodeId) -> Option<String> {
        let path = snapshot.path(id).ok()??;
        if snapshot.resolve_path(&path).ok() == Some(id) {
            Some(path)
        } else {
            Self::id_path(snapshot, id)
        }
    }

    fn id_path(snapshot: &Snapshot, mut id: NodeId) -> Option<String> {
        let mut components = Vec::new();
        while id != NodeId::ROOT {
            components.push(format!("#{}", id.raw()));
            id = snapshot.node(id)?.parent();
        }
        components.reverse();
        Some(format!("/{}", components.join("/")))
    }

    fn one_line(value: &str) -> String {
        value.replace(['\r', '\n'], " ")
    }

    fn output_error(error: io::Error) -> McpError {
        McpError::io("", error)
    }

    fn encoding_error(error: serde_json::Error) -> McpError {
        Self::output_error(io::Error::other(error))
    }
}
