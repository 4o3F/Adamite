//! Text and JSON report rendering without terminal or standard-output ownership.

/// JSON serialization into a caller-supplied writer.
pub mod json;
mod text;

use crate::ReportError;
use serde::Serialize;
use std::io::{self, Write};

/// A serializable report with a terminal text representation.
pub trait Report: Serialize {
    /// Writes text using raw byte counts when requested.
    ///
    /// # Errors
    /// Returns any error from the supplied writer.
    fn write_text(&self, stdout: &mut impl Write, raw_bytes: bool) -> io::Result<()>;
}

/// Selects JSON or text rendering for a caller-supplied writer.
#[derive(Debug)]
pub struct Renderer {
    json: bool,
    raw_bytes: bool,
}

impl Renderer {
    /// Selects JSON output and raw byte counts for text output.
    pub fn new(json: bool, raw_bytes: bool) -> Self {
        Self { json, raw_bytes }
    }

    /// Writes a report as one JSON line or as formatted text.
    ///
    /// # Errors
    /// Returns a serialization error or an error from the supplied writer.
    pub fn write(&self, report: &impl Report, stdout: &mut impl Write) -> Result<(), ReportError> {
        let _entered = tracing::info_span!("render").entered();
        if self.json {
            json::Json(report).write(stdout)
        } else {
            report
                .write_text(stdout, self.raw_bytes)
                .map_err(|source| ReportError::Output { source })
        }
    }
}
