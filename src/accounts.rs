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
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};

use crate::config::{write_private, Config};

pub const FILE_NAME: &str = "accounts.json";

/// Serializes read-modify-write cycles on `accounts.json`. Web handlers and
/// inline callbacks run on different tasks, so without this lock two account
/// additions could load the same snapshot and the last writer would silently
/// drop the other account.
static REGISTRY_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

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
                // Nanoseconds keep two corrupt loads in the same second from
                // overwriting each other's preserved copy.
                let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S-%f");
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

    /// Atomically load, modify and save the registry.
    ///
    /// Concurrent additions/switches are serialized in-process, and the file
    /// itself is replaced with `write_private` (fsync + rename), so a crash
    /// leaves either the old or the new complete registry.
    pub fn update<F>(data_root: &Path, change: F) -> Result<Self>
    where
        F: FnOnce(&mut Registry) -> Result<()>,
    {
        let _guard = REGISTRY_LOCK
            .lock()
            .map_err(|_| anyhow!("account registry lock is poisoned"))?;
        let mut registry = Self::load(data_root);
        change(&mut registry)?;
        registry.save(data_root)?;
        Ok(registry)
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

        let preserved: Vec<_> = std::fs::read_dir(&root)
            .expect("read root")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().contains(".corrupt-"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(preserved.len(), 1, "corrupted registry must be kept");
        assert_eq!(
            std::fs::read_to_string(&preserved[0]).expect("read preserved registry"),
            "{ broken",
            "the preserved registry must be an exact copy"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ignores_a_temporary_registry_left_by_a_crash() {
        let root = temp_root();
        Registry::update(&root, |registry| {
            registry.upsert(Account {
                id: "stable".to_owned(),
                phone: "+79000000000".to_owned(),
                name: "Stable".to_owned(),
                username: None,
                tg_id: 1,
            });
            Ok(())
        })
        .expect("save registry");

        let stale = root.join(format!(".{FILE_NAME}.99999.0.tmp"));
        std::fs::write(&stale, "{ half").expect("write stale temp");

        let registry = Registry::load(&root);
        assert_eq!(registry.accounts.len(), 1);
        assert!(registry.get("stable").is_some());
        let _ = std::fs::remove_file(stale);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_updates_keep_every_account() {
        let root = temp_root();
        let mut threads = Vec::new();
        for index in 0..8 {
            let root = root.clone();
            threads.push(std::thread::spawn(move || {
                Registry::update(&root, |registry| {
                    registry.upsert(Account {
                        id: format!("acc{index}"),
                        phone: format!("+7900000000{index}"),
                        name: format!("Account {index}"),
                        username: None,
                        tg_id: index,
                    });
                    Ok(())
                })
                .expect("update registry");
            }));
        }
        for thread in threads {
            thread.join().expect("join writer");
        }

        let registry = Registry::load(&root);
        assert_eq!(registry.accounts.len(), 8);
        for index in 0..8 {
            assert!(
                registry.get(&format!("acc{index}")).is_some(),
                "account acc{index} was lost"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_failed_update_leaves_the_registry_untouched() {
        let root = temp_root();
        Registry::update(&root, |registry| {
            registry.upsert(Account {
                id: "keep".to_owned(),
                phone: "+79000000000".to_owned(),
                name: "Keep me".to_owned(),
                username: None,
                tg_id: 1,
            });
            Ok(())
        })
        .expect("seed registry");
        let before = std::fs::read_to_string(Registry::path(&root)).expect("read registry");

        let err = Registry::update(&root, |registry| {
            registry.upsert(Account {
                id: "doomed".to_owned(),
                phone: String::new(),
                name: String::new(),
                username: None,
                tg_id: 2,
            });
            Err(anyhow!("boom"))
        })
        .expect_err("update must fail");
        assert!(err.to_string().contains("boom"));

        let after = std::fs::read_to_string(Registry::path(&root)).expect("read registry");
        assert_eq!(before, after, "a failed update must not rewrite the file");
        assert!(Registry::load(&root).get("doomed").is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn active_accounts_use_isolated_database_and_session_paths() {
        let root = temp_root();
        let id = "79001234567";
        let dir = account_dir(&root, id);
        std::fs::create_dir_all(&dir).expect("create account dir");
        std::fs::write(account_session_path(&root, id), b"session").expect("write session");

        Registry::update(&root, |registry| {
            registry.upsert(Account {
                id: id.to_owned(),
                phone: "+79001234567".to_owned(),
                name: "Primary".to_owned(),
                username: None,
                tg_id: 1,
            });
            registry.active = Some(id.to_owned());
            Ok(())
        })
        .expect("save registry");

        let (db, session) = active_paths(&root);
        assert_eq!(db, account_db_path(&root, id));
        assert_eq!(session, account_session_path(&root, id));
        assert_ne!(db, Config::db_path(&root));
        assert_ne!(session, Config::session_path(&root));
        assert!(db.starts_with(&root));
        assert!(session.starts_with(&root));

        // Path traversal in a corrupted/foreign id must stay inside accounts/.
        let escaped = account_dir(&root, "../../etc/passwd");
        assert!(escaped.starts_with(root.join("accounts")));
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
