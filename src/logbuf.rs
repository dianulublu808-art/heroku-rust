// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! In-memory log ring buffer used by the `.logs` command.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;

const LIMIT: usize = 500;

#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogBuffer {
    pub fn push(&self, line: impl Into<String>) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.push_back(line.into());
            while lines.len() > LIMIT {
                lines.pop_front();
            }
        }
    }

    pub fn tail(&self, count: usize) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| {
                let skip = lines.len().saturating_sub(count);
                lines.iter().skip(skip).cloned().collect()
            })
            .unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.clear();
        }
    }

    pub fn make_writer(&self) -> TeeWriter {
        TeeWriter {
            buffer: self.clone(),
            line: String::new(),
        }
    }
}

pub struct TeeWriter {
    buffer: LogBuffer,
    line: String,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = io::stderr().write_all(buf);
        let text = String::from_utf8_lossy(buf);
        for ch in text.chars() {
            if ch == '\n' {
                let line = std::mem::take(&mut self.line);
                if !line.trim().is_empty() {
                    self.buffer.push(line);
                }
            } else {
                self.line.push(ch);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stderr().flush()
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        if !self.line.trim().is_empty() {
            self.buffer.push(std::mem::take(&mut self.line));
        }
    }
}

#[derive(Clone)]
pub struct TeeMaker(pub LogBuffer);

impl<'a> MakeWriter<'a> for TeeMaker {
    type Writer = TeeWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.0.make_writer()
    }
}
