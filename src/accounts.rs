// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Multi-account registry.
//!
//! Every account keeps its own Telegram session and database under
//! `accounts/<id>/`. The registry file (`accounts.json`) records the known
//! accounts and which one is currently active. On startup the bot resolves the
//! database and session paths through this registry, so switching accounts is
//! just a matter of changing the active entry and restarting the process.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{write_private, Config};

pub const FILE_NAME: &str = "accounts.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// Stable filesystem-safe identifier (phone digits or `tg<id>`).
    pub id: String,
    pub phone: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub tg_id: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Registry {
    pub fn path(data_root: &Path) -> PathBuf {
        data_root.join(FILE_NAME)
    }

    pub fn load(data_root: &Path) -> Self {
        let path = Self::path(data_root);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str(&raw) {
            Ok(registry) => registry,
            Err(err) => {
                tracing::warn!(
                    "accounts registry {} is corrupted ({err}); preserving it and starting empty",
                    path.display()
                );
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                let backup = path.with_file_name(format!("{FILE_NAME}.corrupt-{stamp}"));
                match std::fs::rename(&path, &backup) {
                    Ok(()) => tracing::warn!(
                        "Corrupted account registry preserved as {}",
                        backup.display()
                    ),
                    Err(rename_err) => {
                        tracing::error!("Failed to preserve {}: {rename_err}", path.display())
                    }
                }
                Self::default()
            }
        }
    }

    pub fn save(&self, data_root: &Path) -> Result<()> {
        let path = Self::path(data_root);
        let raw = serde_json::to_string_pretty(self)?;
        write_private(&path, raw.as_bytes())
            .with_context(|| format!("failed to save {}", path.display()))
    }

    pub fn get(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|account| account.id == id)
    }

    pub fn get_by_phone(&self, phone: &str) -> Option<&Account> {
        let needle = digits(phone);
        self.accounts
            .iter()
            .find(|account| account.phone == phone || digits(&account.phone) == needle)
    }

    pub fn active_account(&self) -> Option<&Account> {
        self.active.as_deref().and_then(|id| self.get(id))
    }

    pub fn upsert(&mut self, account: Account) {
        if let Some(existing) = self
            .accounts
            .iter_mut()
            .find(|existing| existing.id == account.id)
        {
            *existing = account;
        } else {
            self.accounts.push(account);
        }
    }
}

pub fn account_dir(data_root: &Path, id: &str) -> PathBuf {
    data_root.join("accounts").join(sanitize_id(id))
}

pub fn account_db_path(data_root: &Path, id: &str) -> PathBuf {
    account_dir(data_root, id).join("heroku.json")
}

pub fn account_session_path(data_root: &Path, id: &str) -> PathBuf {
    account_dir(data_root, id).join("heroku.session")
}

/// Resolve the database/session pair for the currently active account.
///
/// Returns the default (top-level) paths when no account is active or when the
/// active account's session file is missing, so a broken registry never bricks
/// the userbot.
pub fn active_paths(data_root: &Path) -> (PathBuf, PathBuf) {
    let registry = Registry::load(data_root);
    if let Some(active) = registry.active.as_deref() {
        match registry.get(active) {
            Some(account) => {
                let session = account_session_path(data_root, &account.id);
                if session.exists() {
                    return (account_db_path(data_root, &account.id), session);
                }
                tracing::warn!(
                    "Active account {} has no session at {}; using the default account",
                    account.id,
                    session.display()
                );
            }
            None => tracing::warn!(
                "Active account {:?} is missing from accounts.json; using the default account",
                active
            ),
        }
    }
    (Config::db_path(data_root), Config::session_path(data_root))
}

/// Build a stable identifier for a phone number: `+7 (900) 12-34-567` → `79001234567`.
pub fn account_id(phone: &str) -> String {
    let digits = digits(phone);
    if digits.is_empty() {
        format!("acc{}", rand::random::<u32>())
    } else {
        digits
    }
}

pub fn sanitize_id(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
        .collect();
    if cleaned.is_empty() {
        "default".to_owned()
    } else {
        cleaned
    }
}

fn digits(raw: &str) -> String {
    raw.chars().filter(char::is_ascii_digit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "heroku-accounts-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn builds_stable_ids_from_phones() {
        assert_eq!(account_id("+7 (900) 12-34-567"), "79001234567");
        assert!(account_id("").starts_with("acc"));
        assert_eq!(sanitize_id("../evil id"), "evilid");
    }

    #[test]
    fn preserves_a_corrupted_registry() {
        let root = temp_root();
        let path = Registry::path(&root);
        std::fs::write(&path, "{ broken").expect("write fixture");

        let registry = Registry::load(&root);
        assert!(registry.accounts.is_empty());
        assert!(!path.exists(), "corrupted registry must be moved aside");

        let preserved = std::fs::read_dir(&root)
            .expect("read root")
            .filter_map(|entry| entry.ok())
            .any(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(preserved, "corrupted registry must be kept");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn falls_back_when_active_session_is_missing() {
        let root = temp_root();
        let registry = Registry {
            active: Some("79001234567".to_owned()),
            accounts: vec![Account {
                id: "79001234567".to_owned(),
                phone: "+79001234567".to_owned(),
                name: String::new(),
                username: None,
                tg_id: 0,
            }],
        };
        registry.save(&root).expect("save registry");

        let (db, session) = active_paths(&root);
        assert_eq!(db, Config::db_path(&root));
        assert_eq!(session, Config::session_path(&root));
        let _ = std::fs::remove_dir_all(root);
    }
}
