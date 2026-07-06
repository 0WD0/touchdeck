use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::engine::TraceEvent;

pub(crate) struct TraceRecorder {
    file: File,
}

impl TraceRecorder {
    pub(crate) fn new(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open trace file {}", path.display()))?;
        Ok(Self { file })
    }

    pub(crate) fn record(&mut self, event: &TraceEvent) -> Result<()> {
        serde_json::to_writer(&mut self.file, event).context("write trace event")?;
        self.file.write_all(b"\n").context("write trace newline")?;
        Ok(())
    }
}
