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

    /// Select log lines at or above `min_level`, optionally filtered by tracing
    /// target (module path). Mirrors the Python `.logs` filtering.
    pub fn select(&self, min_level: i32, modules: &[String]) -> Vec<String> {
        self.lines
            .lock()
            .map(|lines| {
                lines
                    .iter()
                    .filter(|line| {
                        let (level, target) = parse(line);
                        level >= min_level
                            && (modules.is_empty()
                                || modules.iter().any(|module| target_matches(target, module)))
                    })
                    .cloned()
                    .collect()
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

/// Whether a log target belongs to the requested module.
///
/// Built-in modules use their file path (`heroku::modules::loader`), while an
/// external file `modules/example.rs` is compiled as `heroku::ext_example`.
/// `.logs ExampleMod`, `.logs example` and `.logs ext_example` all select it.
fn target_matches(target: &str, module: &str) -> bool {
    let target = target.to_lowercase();
    let module = module.to_lowercase();
    // Match the module path exactly: `settings` must not also select
    // `heroku_settings`.
    if target == module || target.ends_with(&format!("::{module}")) {
        return true;
    }
    let stem = module.strip_suffix("mod").unwrap_or(&module);
    if stem.is_empty() {
        return false;
    }
    target.ends_with(&format!("::ext_{stem}"))
}

/// Parse a formatted tracing line into a numeric severity and target.
fn parse(line: &str) -> (i32, &str) {
    for (token, level) in [
        ("TRACE", 5),
        ("DEBUG", 10),
        ("INFO", 20),
        ("WARN", 30),
        ("ERROR", 40),
    ] {
        if let Some(index) = line.find(&format!(" {token} ")) {
            let rest = &line[index + token.len() + 2..];
            let target = rest.split_once(": ").map(|(name, _)| name).unwrap_or(rest);
            return (level, target.trim());
        }
    }
    (5, "")
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

#[cfg(test)]
mod tests {
    use super::target_matches;

    #[test]
    fn matches_builtin_and_external_targets() {
        assert!(target_matches("heroku::modules::loader", "loader"));
        assert!(target_matches("heroku_settings", "heroku_settings"));
        assert!(!target_matches("heroku_settings", "settings"));
        assert!(target_matches("heroku::ext_example", "examplemod"));
        assert!(target_matches("heroku::ext_example", "example"));
        assert!(!target_matches("heroku::ext_exampletwo", "example"));
    }
}
