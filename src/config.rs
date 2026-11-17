// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Configuration, data-directory layout and the interactive first-run wizard.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const APP_NAME: &str = "Heroku";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api_id: i32,
    pub api_hash: String,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default = "default_lang")]
    pub lang: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_session_name")]
    pub session_name: String,
    #[serde(default)]
    pub inline_bot_token: Option<String>,
    #[serde(default)]
    pub inline_bot_username: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub web: Option<WebConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_host")]
    pub host: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
    #[serde(default)]
    pub password: Option<String>,
}

fn default_lang() -> String {
    "en".to_owned()
}
fn default_log_level() -> String {
    "info".to_owned()
}
fn default_session_name() -> String {
    "heroku".to_owned()
}
fn default_web_host() -> String {
    "127.0.0.1".to_owned()
}
fn default_web_port() -> u16 {
    8080
}

#[derive(Debug, Clone, Default)]
pub struct Cli {
    pub data_root: Option<PathBuf>,
    pub wipe: bool,
    pub version: bool,
    pub no_git: bool,
    pub qr: bool,
}

impl Cli {
    pub fn parse() -> Self {
        let mut cli = Self::default();
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--data-root" => {
                    if let Some(value) = args.next() {
                        cli.data_root = Some(PathBuf::from(value));
                    }
                }
                value if value.starts_with("--data-root=") => {
                    cli.data_root = Some(PathBuf::from(value.trim_start_matches("--data-root=")));
                }
                "-w" | "--wipe" => cli.wipe = true,
                "-v" | "--version" => cli.version = true,
                "--no-git" => cli.no_git = true,
                "-q" | "--qr" => cli.qr = true,
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {}
            }
        }
        cli
    }
}

fn print_help() {
    println!(
        "{APP_NAME} {VERSION} — Telegram userbot\n\n\
         USAGE:\n    heroku [OPTIONS]\n\n\
         OPTIONS:\n\
         \x20   --data-root <PATH>   Directory with config, sessions and database\n\
         \x20   -w, --wipe            Delete config, sessions, database and modules\n\
         \x20   -q, --qr              Sign in by scanning a QR code\n\
         \x20   --no-git              Do not use git for self-updates\n\
         \x20   -v, --version         Print version\n\
         \x20   -h, --help            Print this help\n"
    );
}

pub fn resolve_data_root(cli: &Cli) -> PathBuf {
    if let Some(root) = cli.data_root.clone() {
        return root;
    }
    if let Ok(root) = std::env::var("HEROKU_DATA_ROOT") {
        if !root.trim().is_empty() {
            return PathBuf::from(root);
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

impl Config {
    pub fn path(data_root: &Path) -> PathBuf {
        data_root.join("config.json")
    }

    pub fn sessions_dir(data_root: &Path) -> PathBuf {
        data_root.join("sessions")
    }

    pub fn session_path(data_root: &Path) -> PathBuf {
        Self::sessions_dir(data_root).join("heroku.session")
    }

    pub fn db_path(data_root: &Path) -> PathBuf {
        data_root.join("heroku.json")
    }

    pub fn backup_dir(data_root: &Path) -> PathBuf {
        data_root.join("backups")
    }

    pub fn load_or_setup(data_root: &Path) -> Result<Self> {
        let path = Self::path(data_root);
        let mut config = if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str::<Self>(&raw)
                .with_context(|| format!("failed to parse {}", path.display()))?
        } else {
            Self::wizard(&path)?
        };

        // Environment variables have the highest priority and are convenient in Docker.
        if let Ok(id) = std::env::var("TG_API_ID") {
            if let Ok(id) = id.parse() {
                config.api_id = id;
            }
        }
        if let Ok(hash) = std::env::var("TG_API_HASH") {
            if !hash.trim().is_empty() {
                config.api_hash = hash.trim().to_owned();
            }
        }
        if let Ok(phone) = std::env::var("TG_PHONE") {
            if !phone.trim().is_empty() {
                config.phone = Some(phone.trim().to_owned());
            }
        }
        if let Ok(lang) = std::env::var("HEROKU_LANG") {
            if !lang.trim().is_empty() {
                config.lang = lang.trim().to_lowercase();
            }
        }
        if let Ok(level) = std::env::var("LOG_LEVEL") {
            if !level.trim().is_empty() {
                config.log_level = level;
            }
        }

        if config.api_id == 0 || config.api_hash.trim().is_empty() {
            bail!(
                "api_id/api_hash are not configured. Fill config.json or set TG_API_ID/TG_API_HASH"
            );
        }

        fs::create_dir_all(Self::sessions_dir(data_root))?;
        fs::create_dir_all(Self::backup_dir(data_root))?;
        config.save(data_root)?;
        Ok(config)
    }

    pub fn save(&self, data_root: &Path) -> Result<()> {
        let path = Self::path(data_root);
        let raw = serde_json::to_string_pretty(self)?;
        write_private(&path, raw.as_bytes())?;
        Ok(())
    }

    fn wizard(path: &Path) -> Result<Self> {
        println!(
            "\n\u{1f680} Welcome to {APP_NAME} (Rust edition)!\n\
             Get API credentials at https://my.telegram.org/apps\n"
        );

        let api_id: i32 = loop {
            let raw = prompt("Enter API ID: ")?;
            match raw.trim().parse::<i32>() {
                Ok(id) if id > 0 => break id,
                _ => println!("API ID must be a positive integer."),
            }
        };
        let api_hash = loop {
            let raw = prompt("Enter API hash: ")?;
            let raw = raw.trim().to_owned();
            if raw.len() >= 16 && raw.chars().all(|c| c.is_ascii_hexdigit()) {
                break raw;
            }
            println!("API hash must be a hexadecimal string.");
        };
        let phone = prompt("Enter phone number in international format (+...): ")?;
        let phone = phone.trim();
        let language = prompt("Interface language / Язык интерфейса (en/ru) [en]: ")?;
        let lang = match language.trim().to_lowercase() {
            lang if lang.starts_with("ru") || lang.starts_with("uk") => "ru".to_owned(),
            _ => default_lang(),
        };
        let config = Self {
            api_id,
            api_hash,
            phone: (!phone.is_empty()).then(|| phone.to_owned()),
            lang,
            log_level: default_log_level(),
            session_name: default_session_name(),
            inline_bot_token: None,
            inline_bot_username: None,
            proxy: None,
            web: None,
        };

        let raw = serde_json::to_string_pretty(&config)?;
        write_private(path, raw.as_bytes())?;
        println!("Configuration saved to {}\n", path.display());
        Ok(config)
    }
}

pub fn prompt(message: &str) -> Result<String> {
    print!("{message}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

pub fn prompt_secret(message: &str) -> Result<String> {
    // No external crates: read a line and clear the terminal line after it.
    print!("{message}");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    println!();
    Ok(line.trim().to_owned())
}

/// Atomically replace `path` with `data`, keeping the file private (`0600`).
///
/// The payload is written to a uniquely named temporary file in the same
/// directory, flushed to disk, and then renamed over the destination. Unique
/// names matter because several tasks may save different files - and even the
/// same database - at once; a shared `.tmp` name would truncate a concurrent
/// write and could publish a half-written file.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data");
    let tmp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let write = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        Ok(())
    })();

    if write.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write?;

    // Best effort: make the rename itself durable across a power loss.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if let Ok(directory) = fs::File::open(parent) {
            let _ = directory.sync_all();
        }
    }
    Ok(())
}

pub fn wipe(data_root: &Path) -> Result<()> {
    println!(
        "Are you sure you want to completely delete config, session, database and backups? [y/N]"
    );
    let answer = prompt("> ")?;
    if !matches!(answer.to_lowercase().as_str(), "y" | "yes") {
        println!("Cancelled");
        return Ok(());
    }

    let mut removed = 0;
    for entry in fs::read_dir(data_root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if (name.starts_with("config")
            || name.ends_with(".session")
            || name == "heroku.json"
            || name == "accounts.json")
            && entry.path().is_file()
        {
            fs::remove_file(entry.path())?;
            removed += 1;
        }
    }
    for dir in ["sessions", "backups", "plugins", "accounts", "modules"] {
        let path = data_root.join(dir);
        if path.is_dir() {
            fs::remove_dir_all(path)?;
            removed += 1;
        }
    }
    println!("Removed: {removed}");
    Ok(())
}
