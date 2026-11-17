// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! In-memory log ring buffer used by the `.logs` command.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use regex::Regex;
use tracing_subscriber::fmt::MakeWriter;

const LIMIT: usize = 500;

/// Key/value pairs whose value must never reach `.logs`: API hashes, bot
/// tokens, passwords and session strings. Compiled lazily and stored as
/// `Option` so a bad pattern can never panic the logger.
static SECRET_ASSIGNMENT: Lazy<Option<Regex>> = Lazy::new(|| {
    Regex::new(
        r#"(?i)\b(api_hash|bot_token|access_token|refresh_token|session_string|password|passwd|secret|token|cookie)\b(\s*[:=]\s*)("[^"]*"|'[^']*'|[^\s,;&]+)"#,
    )
    .ok()
});

static BEARER: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}").ok());

/// Telegram bot tokens look like `123456789:AA...` (35+ characters after the
/// colon). Redact them even when no key name is present.
static BOT_TOKEN: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r"\b\d{6,12}:[A-Za-z0-9_-]{30,}\b").ok());

/// `scheme://user:password@host` credentials.
static URL_CREDENTIALS: Lazy<Option<Regex>> =
    Lazy::new(|| Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://[^/\s:@]+):([^@\s/]+)@").ok());

/// Remove secrets from a log line before it is stored in the ring buffer.
/// Formatting is preserved for everything that is not a secret, so `.logs`
/// stays useful for debugging.
pub fn redact(line: &str) -> String {
    let mut out = line.to_owned();
    if let Some(regex) = BEARER.as_ref() {
        out = regex.replace_all(&out, "${1}<redacted>").into_owned();
    }
    if let Some(regex) = SECRET_ASSIGNMENT.as_ref() {
        out = regex.replace_all(&out, "${1}${2}<redacted>").into_owned();
    }
    if let Some(regex) = BOT_TOKEN.as_ref() {
        out = regex.replace_all(&out, "<redacted-token>").into_owned();
    }
    if let Some(regex) = URL_CREDENTIALS.as_ref() {
        out = regex.replace_all(&out, "${1}:<redacted>@").into_owned();
    }
    out
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl LogBuffer {
    pub fn push(&self, line: impl Into<String>) {
        if let Ok(mut lines) = self.lines.lock() {
            lines.push_back(redact(&line.into()));
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
    use super::{redact, target_matches};

    #[test]
    fn matches_builtin_and_external_targets() {
        assert!(target_matches("heroku::modules::loader", "loader"));
        assert!(target_matches("heroku_settings", "heroku_settings"));
        assert!(!target_matches("heroku_settings", "settings"));
        assert!(target_matches("heroku::ext_example", "examplemod"));
        assert!(target_matches("heroku::ext_example", "example"));
        assert!(!target_matches("heroku::ext_exampletwo", "example"));
    }

    #[test]
    fn redacts_known_secret_shapes() {
        let line = "INFO heroku::login: api_hash=0123456789abcdef token: \"abc.def\" password = 'hunter2' session_string=1BVtsOK";
        let cleaned = redact(line);
        assert!(!cleaned.contains("0123456789abcdef"));
        assert!(!cleaned.contains("abc.def"));
        assert!(!cleaned.contains("hunter2"));
        assert!(!cleaned.contains("1BVtsOK"));
        assert!(cleaned.contains("api_hash=<redacted>"));
        assert!(cleaned.contains("token: <redacted>"));
        assert!(cleaned.contains("password = <redacted>"));
    }

    #[test]
    fn redacts_bearer_tokens_and_bot_tokens() {
        let bearer = redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.signature");
        assert!(!bearer.contains("eyJhbGciOiJIUzI1NiJ9"));
        assert!(bearer.contains("Bearer <redacted>"));

        let bot = redact("connecting with 123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ");
        assert!(!bot.contains("AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsawQ"));
        assert!(bot.contains("<redacted-token>"));
    }

    #[test]
    fn redacts_url_credentials_but_keeps_the_host() {
        let cleaned = redact("fetching https://user:s3cret@example.com/hook?x=1");
        assert!(!cleaned.contains("s3cret"));
        assert!(cleaned.contains("https://user:<redacted>@example.com/hook?x=1"));
    }

    #[test]
    fn leaves_ordinary_lines_untouched() {
        let line = "INFO heroku::dispatcher: command .help handled in 12ms";
        assert_eq!(redact(line), line);
    }
}
