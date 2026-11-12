// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! A small, thread-safe JSON database compatible with the original Heroku
//! `config-<id>.json` layout (owner -> key -> value).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use anyhow::{Context, Result};
use serde_json::{Map, Value};

use crate::config::write_private;

pub struct Database {
    path: PathBuf,
    data: RwLock<Map<String, Value>>,
    dirty: AtomicBool,
}

impl Database {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut map = Map::new();
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .with_context(|| format!("failed to read database {}", path.display()))?;
            if !raw.trim().is_empty() {
                match serde_json::from_str::<Value>(&raw) {
                    Ok(Value::Object(object)) => map = object,
                    Ok(_) => tracing::warn!("Database is not a JSON object, creating a new one"),
                    Err(err) => tracing::warn!("Database is corrupted: {err}; creating a new one"),
                }
            }
        }

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
            }
        }

        Ok(Self {
            path,
            data: RwLock::new(map),
            dirty: AtomicBool::new(false),
        })
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
        let _ = std::fs::remove_file(path);
    }
}
