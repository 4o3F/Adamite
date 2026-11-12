use crate::{McpError, SnapshotInfo, parameters::Format};
use adamite_access::{DirectoryLookup, ReportError, report::Envelope};
use adamite_core::snapshot::{NodeId, QueryError, Snapshot};
use adamite_format::{Age, Escaped, ScanTime};
use rmcp::model::{CallToolResult, ContentBlock};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::Path,
    time::{Duration, SystemTime},
};

/// Maximum encoded MCP result size, including duplicated structured content.
const OUTPUT_CAP: usize = 32_768;

/// A complete response assembled before wire encoding; the item cap belongs here.
#[derive(Debug)]
pub(crate) struct Response {
    text: String,
    json: Value,
    paging: Option<(usize, usize)>,
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
        if json.get("truncated").is_none() {
            json["truncated"] = json!(false);
        }
        json["next_offset"] = Value::Null;
        Ok(Self {
            text: String::from_utf8(bytes)
                .map_err(|error| Self::output_error(io::Error::other(error)))?,
            json,
            paging: None,
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
            paging: None,
        })
    }

    pub(crate) fn page(&mut self, offset: usize, total: usize) {
        self.paging = Some((offset, total));
    }

    pub(crate) fn finish(mut self, format: Format) -> CallToolResult {
        let key = if self.json.get("rows").is_some() {
            "rows"
        } else {
            "items"
        };
        let items = self.json[key].as_array().cloned().unwrap_or_default();
        let mut count = items.len();
        let mut fitting = 0;
        let mut oversized = count.saturating_add(1);
        // Every report renderer escapes embedded newlines and writes one line per item.
        let mut lines: Vec<String> = self.text.lines().map(str::to_owned).collect();
        let prefix = lines.len().saturating_sub(count);
        loop {
            let mut text = lines
                .iter()
                .take(prefix.saturating_add(count))
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n");
            text.push('\n');
            if self.json["truncated"] == true {
                text.push_str("truncated\n");
            }
            if let Some((offset, total)) = self.paging {
                let end = offset.saturating_add(count);
                self.json["next_offset"] = if end < total { json!(end) } else { Value::Null };
                if end < total {
                    text.push_str(&format!(
                        "showing {}–{end} of {total} · next offset={end}\n",
                        offset.saturating_add(1)
                    ));
                }
            }
            let result = match format {
                Format::Text => CallToolResult::success(vec![ContentBlock::text(&text)]),
                Format::Json => CallToolResult::structured(self.json.clone()),
            };
            // Include the MCP result wrapper and the duplicated structured JSON in the cap.
            if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= OUTPUT_CAP) {
                if count.saturating_add(1) >= oversized {
                    return result;
                }
                fitting = count;
                count = fitting.saturating_add(oversized.saturating_sub(fitting) / 2);
                self.json[key] = json!(&items[..count]);
                continue;
            }
            self.json["truncated"] = json!(true);
            if count > 1 {
                oversized = count;
                count = fitting
                    .saturating_add(oversized.saturating_sub(fitting) / 2)
                    .max(1);
                self.json[key] = json!(&items[..count]);
            } else {
                // Preserve an oversized single item; shrink its strings at UTF-8 boundaries.
                oversized = count.saturating_add(1);
                let longest = Self::longest_string(&self.json);
                Self::shorten_value(&mut self.json, longest);
                let longest = lines
                    .iter()
                    .take(prefix.saturating_add(count))
                    .map(String::len)
                    .max()
                    .unwrap_or(0);
                for line in lines.iter_mut().take(prefix.saturating_add(count)) {
                    if line.len() == longest {
                        *line = Self::shorten(line, longest.div_ceil(2));
                    }
                }
            }
        }
    }

    pub(crate) fn error(error: McpError, format: Format) -> CallToolResult {
        let mut message = format!(
            "error[{}]: {}\nnext: {}",
            error.code(),
            Self::one_line(&error.to_string()),
            Self::one_line(&error.next_step())
        );
        loop {
            let result = match format {
                Format::Text => CallToolResult::error(vec![ContentBlock::text(&message)]),
                Format::Json => CallToolResult::structured_error(
                    json!({"schema": "adamite/1", "error": message}),
                ),
            };
            if serde_json::to_vec(&result).is_ok_and(|bytes| bytes.len() <= OUTPUT_CAP) {
                return result;
            }
            let longest = message.lines().map(str::len).max().unwrap_or(0);
            message = message
                .lines()
                .map(|line| {
                    if line.len() == longest {
                        Self::shorten(line, longest.div_ceil(2))
                    } else {
                        line.to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
        }
    }

    fn shorten(value: &str, limit: usize) -> String {
        if value.len() <= limit {
            return value.to_owned();
        }
        let half = limit.saturating_sub('…'.len_utf8()) / 2;
        let mut start = half;
        while !value.is_char_boundary(start) {
            start = start.saturating_sub(1);
        }
        let mut end = value.len().saturating_sub(half);
        while !value.is_char_boundary(end) {
            end = end.saturating_add(1);
        }
        format!("{}…{}", &value[..start], &value[end..])
    }

    fn longest_string(value: &Value) -> usize {
        match value {
            Value::String(text) => text.len(),
            Value::Array(items) => items.iter().map(Self::longest_string).max().unwrap_or(0),
            Value::Object(fields) => fields.values().map(Self::longest_string).max().unwrap_or(0),
            _ => 0,
        }
    }

    fn shorten_value(value: &mut Value, longest: usize) {
        match value {
            Value::String(text) if text.len() == longest => {
                *text = Self::shorten(text, longest.div_ceil(2))
            }
            Value::Array(items) => items
                .iter_mut()
                .for_each(|item| Self::shorten_value(item, longest)),
            Value::Object(fields) => fields
                .values_mut()
                .for_each(|field| Self::shorten_value(field, longest)),
            _ => {}
        }
    }

    pub(crate) fn range(name: &str, value: i64, maximum: usize) -> Result<usize, McpError> {
        usize::try_from(value)
            .ok()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or_else(|| McpError::InvalidArgument {
                message: format!("{name} {value} is outside the allowed range"),
                allowed: format!("Pass {name} from 1 through {maximum}."),
            })
    }

    pub(crate) fn query_error(error: QueryError) -> McpError {
        let allowed = match &error {
            QueryError::InvalidKind => {
                "Pass kind: video, audio, image, archive, disk-image, document, code, executable, or other."
            }
            QueryError::InvalidGlob { .. } | QueryError::PatternTooLong { .. } => {
                "Use a name glob of at most 256 bytes: * (any characters), ? (one character), [abc] (character class), {a,b} (alternatives), or a plain substring. Backslash escapes glob characters."
            }
            QueryError::InvalidArgument { reason } => reason,
            _ => return McpError::io("", io::Error::other(error)),
        };
        McpError::InvalidArgument {
            message: error.to_string(),
            allowed: allowed.into(),
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
            ReportError::Query {
                source: QueryError::NotADirectory { node },
            } => McpError::NotADirectory {
                path: snapshot
                    .path(node)
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| format!("/#{}", node.raw())),
            },
            ReportError::Query { source } => Self::query_error(source),
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
