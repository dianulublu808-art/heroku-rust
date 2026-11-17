// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! A small, thread-safe JSON database compatible with the original Heroku
//! `config-<id>.json` layout (owner -> key -> value).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::config::write_private;

/// Current on-disk schema version, stored as `heroku.meta.schema`.
///
/// Version 1 is the owner→key→value layout inherited from Python Heroku.
/// Bump this constant and extend [`migrate_schema`] when the layout changes.
pub const SCHEMA_VERSION: u64 = 1;

const META_OWNER: &str = "heroku.meta";
const SCHEMA_KEY: &str = "schema";

pub struct Database {
    path: PathBuf,
    data: RwLock<Map<String, Value>>,
    dirty: AtomicBool,
    /// Serializes snapshots and disk writes so two concurrent saves cannot
    /// interleave and publish an older snapshot over a newer one.
    save_lock: Mutex<()>,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut map = Map::new();
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read database {}", path.display()))?;
            if !raw.trim().is_empty() {
                let parsed = serde_json::from_str::<Value>(&raw).map_err(|err| err.to_string());
                match parsed {
                    Ok(Value::Object(object)) => map = object,
                    other => {
                        let reason = match other {
                            Ok(_) => "the document is not a JSON object".to_owned(),
                            Err(err) => err,
                        };
                        tracing::warn!("Database is corrupted ({reason}); starting empty");
                        // Never silently destroy user data: move the unreadable
                        // file aside so it can be inspected or restored.
                        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                        let backup = path.with_file_name(format!(
                            "{}.corrupt-{stamp}",
                            path.file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("heroku.json")
                        ));
                        match fs::rename(&path, &backup) {
                            Ok(()) => tracing::warn!(
                                "Unreadable database preserved as {}",
                                backup.display()
                            ),
                            Err(err) => tracing::error!(
                                "Failed to preserve the corrupted database {}: {err}",
                                path.display()
                            ),
                        }
                    }
                }

                // Keep an untouched copy of the original file before rewriting
                // it, so a migration bug can never destroy user settings.
                if map
                    .keys()
                    .any(|key| key.starts_with("hikka.") || key.starts_with("legacy."))
                {
                    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    let backup = path.with_file_name(format!(
                        "{}.pre-migration-{stamp}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("heroku.json")
                    ));
                    match write_private(&backup, raw.as_bytes()) {
                        Ok(()) => {
                            tracing::info!("Database backup before migration: {}", backup.display())
                        }
                        Err(err) => tracing::warn!(
                            "Could not back up the database before migration ({}): {err}",
                            backup.display()
                        ),
                    }
                }
            }
        }

        let mut needs_save = false;

        // Migrate old Hikka/GeekTG namespaces, exactly like the Python version did.
        // If both the old and the new namespace exist, merge their keys instead of
        // letting the last migration overwrite the earlier one.
        for (old, new) in [("hikka.", "heroku."), ("legacy.", "heroku.")] {
            let keys: Vec<String> = map
                .keys()
                .filter(|key| key.starts_with(old))
                .cloned()
                .collect();
            for key in keys {
                let Some(value) = map.remove(&key) else {
                    continue;
                };
                let target = key.replacen(old, new, 1);
                match (map.get_mut(&target), value) {
                    (Some(Value::Object(existing)), Value::Object(incoming)) => {
                        for (name, item) in incoming {
                            existing.entry(name).or_insert(item);
                        }
                    }
                    (_, value) => {
                        map.insert(target, value);
                    }
                }
                needs_save = true;
            }
        }

        let stored_schema = map
            .get(META_OWNER)
            .and_then(Value::as_object)
            .and_then(|meta| meta.get(SCHEMA_KEY))
            .and_then(Value::as_u64);
        match stored_schema {
            Some(version) if version > SCHEMA_VERSION => {
                // A newer build wrote this database. Keep the file intact: an
                // older binary must not silently downgrade or drop new keys.
                tracing::warn!(
                    "Database schema v{version} is newer than this build (v{SCHEMA_VERSION}); \
                     refusing to downgrade it"
                );
            }
            Some(version) if version < SCHEMA_VERSION => {
                tracing::info!("Migrating database schema v{version} → v{SCHEMA_VERSION}");
                migrate_schema(&mut map, version);
                set_schema(&mut map);
                needs_save = true;
            }
            Some(_) => {}
            None => {
                set_schema(&mut map);
                needs_save = true;
            }
        }

        let database = Self {
            path,
            data: RwLock::new(map),
            dirty: AtomicBool::new(false),
            save_lock: Mutex::new(()),
        };
        if needs_save {
            // Persist namespace/schema migrations immediately; otherwise they
            // would only reach disk after an unrelated setting is changed.
            database.save();
        }
        Ok(database)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn get_value(&self, owner: &str, key: &str) -> Option<Value> {
        let data = self.data.read().ok()?;
        data.get(owner)?.as_object()?.get(key).cloned()
    }

    pub fn get(&self, owner: &str, key: &str, default: Value) -> Value {
        self.get_value(owner, key).unwrap_or(default)
    }

    pub fn get_str(&self, owner: &str, key: &str, default: &str) -> String {
        self.get_value(owner, key)
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .unwrap_or_else(|| default.to_owned())
    }

    pub fn get_bool(&self, owner: &str, key: &str, default: bool) -> bool {
        self.get_value(owner, key)
            .and_then(|value| value.as_bool())
            .unwrap_or(default)
    }

    pub fn get_i64(&self, owner: &str, key: &str, default: i64) -> i64 {
        self.get_value(owner, key)
            .and_then(|value| value.as_i64())
            .unwrap_or(default)
    }

    pub fn get_string_list(&self, owner: &str, key: &str) -> Vec<String> {
        self.get_value(owner, key)
            .and_then(|value| value.as_array().cloned())
            .map(|array| {
                array
                    .into_iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_i64_list(&self, owner: &str, key: &str) -> Vec<i64> {
        self.get_value(owner, key)
            .and_then(|value| value.as_array().cloned())
            .map(|array| {
                array
                    .into_iter()
                    .filter_map(|value| value.as_i64())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_map(&self, owner: &str, key: &str) -> Map<String, Value> {
        self.get_value(owner, key)
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default()
    }

    pub fn get_array(&self, owner: &str, key: &str) -> Vec<Value> {
        self.get_value(owner, key)
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default()
    }

    pub fn set_value(&self, owner: &str, key: &str, value: Value) -> bool {
        {
            let Ok(mut data) = self.data.write() else {
                return false;
            };
            let entry = data
                .entry(owner.to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            if let Some(object) = entry.as_object_mut() {
                object.insert(key.to_owned(), value);
            }
        }
        self.dirty.store(true, Ordering::Relaxed);
        self.save()
    }

    pub fn set(&self, owner: &str, key: &str, value: impl Into<Value>) -> bool {
        self.set_value(owner, key, value.into())
    }

    pub fn set_owner(&self, owner: &str, value: Map<String, Value>) -> bool {
        {
            let Ok(mut data) = self.data.write() else {
                return false;
            };
            data.insert(owner.to_owned(), Value::Object(value));
        }
        self.save()
    }

    pub fn remove(&self, owner: &str, key: &str) -> bool {
        {
            let Ok(mut data) = self.data.write() else {
                return false;
            };
            if let Some(object) = data.get_mut(owner).and_then(Value::as_object_mut) {
                object.remove(key);
            }
        }
        self.save()
    }

    pub fn clear_owner(&self, owner: &str) -> bool {
        {
            let Ok(mut data) = self.data.write() else {
                return false;
            };
            data.remove(owner);
        }
        self.save()
    }

    pub fn dump(&self) -> Value {
        self.data
            .read()
            .map(|data| Value::Object(data.clone()))
            .unwrap_or(Value::Object(Map::new()))
    }

    pub fn dump_pretty(&self) -> String {
        serde_json::to_string_pretty(&self.dump()).unwrap_or_else(|_| "{}".to_owned())
    }

    pub fn restore(&self, value: Value) -> bool {
        let Value::Object(object) = value else {
            return false;
        };
        if let Ok(mut data) = self.data.write() {
            *data = object;
        }
        self.save()
    }

    pub fn save(&self) -> bool {
        let _guard = self
            .save_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let raw = match self.data.read() {
            Ok(data) => match serde_json::to_string_pretty(&Value::Object(data.clone())) {
                Ok(raw) => raw,
                Err(err) => {
                    tracing::error!("Database serialization failed: {err}");
                    return false;
                }
            },
            Err(_) => return false,
        };

        match write_private(&self.path, raw.as_bytes()) {
            Ok(()) => {
                self.dirty.store(false, Ordering::Relaxed);
                true
            }
            Err(err) => {
                tracing::error!("Database save failed: {err}");
                false
            }
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }
}

fn set_schema(map: &mut Map<String, Value>) {
    let meta = map
        .entry(META_OWNER.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(meta) = meta.as_object_mut() {
        meta.insert(SCHEMA_KEY.to_owned(), Value::from(SCHEMA_VERSION));
    }
}

/// Bring an older database up to [`SCHEMA_VERSION`].
///
/// Each case must be idempotent and must never delete keys it does not
/// understand. Version 1 is the initial Rust/Python-compatible layout, so the
/// only upgrade from v0 is adding the schema marker itself.
fn migrate_schema(_map: &mut Map<String, Value>, from: u64) {
    let mut version = from;
    while version < SCHEMA_VERSION {
        match version {
            0 => {
                // v0 means "written by Python Heroku or by an early Rust build".
                // The layout is already compatible; nothing to rewrite.
            }
            other => {
                tracing::warn!("No migration registered for schema v{other}; leaving data as is");
                break;
            }
        }
        version += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (Database, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "heroku-db-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let db = Database::open(&path).expect("open database");
        (db, path)
    }

    #[test]
    fn stores_and_reads_values() {
        let (db, path) = temp_db();
        assert!(db.set("heroku.main", "prefix", "."));
        assert_eq!(db.get_str("heroku.main", "prefix", "!"), ".");
        assert!(db.set("Mod", "enabled", true));
        assert!(db.get_bool("Mod", "enabled", false));
        assert!(db.set(
            "Mod",
            "ids",
            Value::Array(vec![Value::from(1), Value::from(2)])
        ));
        assert_eq!(db.get_i64_list("Mod", "ids"), vec![1, 2]);
        assert_eq!(db.get_i64("Mod", "missing", 7), 7);

        // Reopen and make sure the JSON survived the round-trip.
        let reopened = Database::open(&path).expect("reopen");
        assert_eq!(reopened.get_str("heroku.main", "prefix", ""), ".");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dumps_and_restores() {
        let (db, path) = temp_db();
        db.set("A", "x", 1);
        db.set("B", "y", "text");

        let dump = db.dump();
        assert!(db.restore(serde_json::json!({"C": {"z": true}})));
        assert_eq!(db.get_value("A", "x"), None);
        assert!(db.get_bool("C", "z", false));

        assert!(db.restore(dump));
        assert_eq!(db.get_i64("A", "x", 0), 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migrates_legacy_namespaces() {
        let path = std::env::temp_dir().join(format!(
            "heroku-legacy-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(
            &path,
            r#"{"hikka.main":{"prefix":"?"},"legacy.main":{"x":1}}"#,
        )
        .expect("write fixture");
        let db = Database::open(&path).expect("open");
        assert_eq!(db.get_str("heroku.main", "prefix", ""), "?");
        assert_eq!(db.get_i64("heroku.main", "x", 0), 1);

        let prefix = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("file name")
            .to_owned();
        let backups: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .expect("read temp dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(&format!("{prefix}.pre-migration-")))
            .collect();
        assert_eq!(backups.len(), 1, "migration must keep a backup");
        for name in backups {
            let _ = std::fs::remove_file(std::env::temp_dir().join(name));
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn preserves_a_corrupted_database() {
        let path = std::env::temp_dir().join(format!(
            "heroku-corrupt-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, "{ definitely not json").expect("write fixture");
        let db = Database::open(&path).expect("open");
        assert_eq!(db.get_i64("missing", "key", 3), 3);

        let prefix = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("file name")
            .to_owned();
        let preserved: Vec<_> = std::fs::read_dir(std::env::temp_dir())
            .expect("read temp dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(&format!("{prefix}.corrupt-")))
            .collect();
        assert_eq!(preserved.len(), 1, "corrupted database must be kept");

        for name in preserved {
            let _ = std::fs::remove_file(std::env::temp_dir().join(name));
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn concurrent_saves_leave_valid_json() {
        use std::sync::Arc;

        let (db, path) = temp_db();
        db.set("heroku.main", "prefix", ".");
        let db = Arc::new(db);
        let mut handles = Vec::new();
        for worker in 0..6 {
            let db = Arc::clone(&db);
            handles.push(std::thread::spawn(move || {
                for value in 0..5 {
                    db.set("heroku.test", &format!("key{worker}"), value);
                }
            }));
        }
        for handle in handles {
            handle.join().expect("worker");
        }
        db.set("heroku.main", "prefix", ".");

        let raw = std::fs::read_to_string(&path).expect("read database");
        let parsed: Value = serde_json::from_str(&raw).expect("valid json after concurrent saves");
        assert!(parsed.is_object());
        assert_eq!(db.get_i64("heroku.test", "key5", -1), 4);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn writes_the_current_schema_version_on_first_open() {
        let (db, path) = temp_db();
        assert_eq!(db.get_i64(META_OWNER, SCHEMA_KEY, 0), SCHEMA_VERSION as i64);
        let raw = std::fs::read_to_string(&path).expect("database was persisted");
        assert!(raw.contains("\"schema\""));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn does_not_downgrade_a_newer_schema() {
        let path = std::env::temp_dir().join(format!(
            "heroku-db-future-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(
            &path,
            r#"{"heroku.meta":{"schema":999},"heroku.main":{"prefix":"."}}"#,
        )
        .expect("write fixture");
        let db = Database::open(&path).expect("open database");
        assert_eq!(db.get_i64(META_OWNER, SCHEMA_KEY, 0), 999);
        assert_eq!(db.get_str("heroku.main", "prefix", ""), ".");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persists_namespace_migration_without_another_write() {
        let path = std::env::temp_dir().join(format!(
            "heroku-db-migrate-{}-{}.json",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::write(&path, r#"{"hikka.main":{"prefix":"?"}}"#).expect("write fixture");
        {
            let db = Database::open(&path).expect("open database");
            assert_eq!(db.get_str("heroku.main", "prefix", ""), "?");
        }
        let raw = std::fs::read_to_string(&path).expect("read migrated database");
        assert!(raw.contains("heroku.main"));
        assert!(!raw.contains("hikka.main"));

        // Only remove this test's own backup: other tests run in parallel and
        // may have their own pre-migration snapshots in the same directory.
        let prefix = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("file name")
            .to_owned();
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with(&format!("{prefix}.pre-migration-")) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let _ = std::fs::remove_file(path);
    }
}
