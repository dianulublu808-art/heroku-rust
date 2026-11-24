// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Shared runtime context available to every module.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use grammers_client::message::{Button, InputMessage, ReplyMarkup};
use grammers_client::Client;
use grammers_session::storages::ErasedSession;
use grammers_session::types::{PeerId, PeerKind, PeerRef};
use serde_json::Value;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::database::Database;
use crate::inline::InlineManager;
use crate::logbuf::LogBuffer;
use crate::module::{
    Modules, ALL, GROUP_ADMIN, GROUP_ADMIN_ADD_ADMINS, GROUP_ADMIN_ANY, GROUP_MEMBER, GROUP_OWNER,
};
use crate::security::Security;
use crate::tg::{escape_html, Msg};
use crate::translations::Translator;

/// Upper bound on tasks a single module may keep alive at once. Modules that
/// spawn per update should let those tasks finish; long-running loops are
/// usually one or two per module.
const MODULE_TASK_LIMIT: usize = 256;

/// Runtime lifecycle of a module.
///
/// `loaded` means `on_load` completed successfully. `failed` keeps the reason a
/// module is unusable so dispatch, help and inline handlers can skip it without
/// retrying the hook on every update.
#[derive(Default)]
pub(crate) struct ModuleLifecycle {
    loaded: HashSet<String>,
    failed: HashMap<String, String>,
}

impl ModuleLifecycle {
    fn key(name: &str) -> String {
        name.to_lowercase()
    }

    pub(crate) fn is_loaded(&self, name: &str) -> bool {
        self.loaded.contains(&Self::key(name))
    }

    pub(crate) fn failure(&self, name: &str) -> Option<String> {
        self.failed.get(&Self::key(name)).cloned()
    }

    pub(crate) fn mark_loaded(&mut self, name: &str) {
        let key = Self::key(name);
        self.failed.remove(&key);
        self.loaded.insert(key);
    }

    pub(crate) fn mark_failed(&mut self, name: &str, reason: String) {
        let key = Self::key(name);
        self.loaded.remove(&key);
        self.failed.insert(key, reason);
    }

    pub(crate) fn mark_unloaded(&mut self, name: &str) {
        let key = Self::key(name);
        self.loaded.remove(&key);
        self.failed.remove(&key);
    }

    pub(crate) fn failed_modules(&self) -> Vec<(String, String)> {
        let mut modules: Vec<(String, String)> = self
            .failed
            .iter()
            .map(|(name, reason)| (name.clone(), reason.clone()))
            .collect();
        modules.sort_by(|left, right| left.0.cmp(&right.0));
        modules
    }
}

/// Tasks spawned by a module through [`Ctx::spawn_module_task`]. They are
/// aborted when the module is unloaded or its `on_load` fails, so a failed
/// reload cannot leave background loops behind.
#[derive(Default)]
pub(crate) struct ModuleTasks {
    tasks: Mutex<HashMap<String, Vec<JoinHandle<()>>>>,
}

impl ModuleTasks {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<JoinHandle<()>>>> {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn spawn<F>(&self, module: &str, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let key = module.to_lowercase();
        let mut tasks = self.lock();
        let handles = tasks.entry(key).or_default();
        handles.retain(|handle| !handle.is_finished());
        if handles.len() >= MODULE_TASK_LIMIT {
            tracing::error!(
                module,
                "Refusing to spawn another task: module already owns {MODULE_TASK_LIMIT} live tasks"
            );
            return false;
        }
        handles.push(tokio::spawn(future));
        true
    }

    pub(crate) fn abort(&self, module: &str) -> usize {
        let handles = self
            .lock()
            .remove(&module.to_lowercase())
            .unwrap_or_default();
        let count = handles.len();
        for handle in handles {
            handle.abort();
        }
        count
    }

    pub(crate) fn abort_all(&self) -> usize {
        let mut tasks = self.lock();
        let mut count = 0usize;
        for (_, handles) in tasks.drain() {
            count += handles.len();
            for handle in handles {
                handle.abort();
            }
        }
        count
    }

    pub(crate) fn count(&self, module: &str) -> usize {
        self.lock()
            .get(&module.to_lowercase())
            .map(|handles| {
                handles
                    .iter()
                    .filter(|handle| !handle.is_finished())
                    .count()
            })
            .unwrap_or_default()
    }
}

pub struct Ctx {
    pub client: Client,
    pub session: Arc<ErasedSession>,
    pub db: Arc<Database>,
    pub config: Config,
    pub data_root: PathBuf,
    pub modules: Modules,
    pub security: Security,
    pub translator: Translator,
    pub log_buffer: LogBuffer,
    pub inline: Arc<InlineManager>,
    pub started: Instant,
    pub me_id: i64,
    pub me_username: Option<String>,
    pub me_name: String,
    /// Loaded/failed module state. Kept in sync by the startup path and
    /// `.togglemod`, so hooks never run twice and broken modules never receive
    /// updates.
    pub(crate) lifecycle: Mutex<ModuleLifecycle>,
    /// Background tasks owned by modules, aborted on unload or failed load.
    pub(crate) module_tasks: ModuleTasks,
}

impl Ctx {
    pub fn new_lifecycle() -> Mutex<ModuleLifecycle> {
        Mutex::new(ModuleLifecycle::default())
    }

    pub fn new_module_tasks() -> ModuleTasks {
        ModuleTasks::default()
    }

    fn lifecycle_state(&self) -> std::sync::MutexGuard<'_, ModuleLifecycle> {
        self.lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn module_loaded(&self, name: &str) -> bool {
        self.lifecycle_state().is_loaded(name)
    }

    pub fn module_failed(&self, name: &str) -> Option<String> {
        self.lifecycle_state().failure(name)
    }

    pub fn failed_modules(&self) -> Vec<(String, String)> {
        self.lifecycle_state().failed_modules()
    }

    pub fn mark_module_loaded(&self, name: &str) {
        self.lifecycle_state().mark_loaded(name);
    }

    pub fn mark_module_failed(&self, name: &str, reason: impl Into<String>) {
        self.lifecycle_state().mark_failed(name, reason.into());
    }

    pub fn mark_module_unloaded(&self, name: &str) {
        self.lifecycle_state().mark_unloaded(name);
    }

    /// Spawn a background task owned by `module`. Prefer this over a bare
    /// `tokio::spawn` inside `on_load`: the runtime aborts owned tasks when the
    /// module is unloaded or fails to load.
    pub fn spawn_module_task<F>(&self, module: &str, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.module_tasks.spawn(module, future)
    }

    pub fn abort_module_tasks(&self, module: &str) -> usize {
        self.module_tasks.abort(module)
    }

    pub fn abort_all_module_tasks(&self) -> usize {
        self.module_tasks.abort_all()
    }

    pub fn module_task_count(&self, module: &str) -> usize {
        self.module_tasks.count(module)
    }

    pub fn tr(&self, key: &str) -> String {
        self.translator.t(key)
    }

    pub fn tf(&self, key: &str, values: &[&str]) -> String {
        self.translator.tf(key, values)
    }

    pub fn tn(&self, key: &str, values: &[(&str, &str)]) -> String {
        self.translator.tn(key, values)
    }

    pub fn uptime(&self) -> String {
        let seconds = self.started.elapsed().as_secs();
        let days = seconds / 86_400;
        let hours = (seconds % 86_400) / 3_600;
        let minutes = (seconds % 3_600) / 60;
        let seconds = seconds % 60;
        if days > 0 {
            format!("{days}d {hours}h {minutes}m")
        } else if hours > 0 {
            format!("{hours}h {minutes}m {seconds}s")
        } else if minutes > 0 {
            format!("{minutes}m {seconds}s")
        } else {
            format!("{seconds}s")
        }
    }

    pub fn prefixes(&self, user_id: i64) -> Vec<String> {
        let mut prefixes = Vec::new();
        let primary = self.db.get_str("heroku.main", "command_prefix", ".");
        if !primary.is_empty() {
            prefixes.push(primary);
        }
        for prefix in self
            .db
            .get_string_list("heroku.main", "command_prefix_aliases")
        {
            if !prefix.is_empty() {
                prefixes.push(prefix);
            }
        }

        if user_id != self.me_id {
            let personal = self.db.get_map("heroku.main", "command_prefixes");
            if let Some(value) = personal.get(&user_id.to_string()) {
                let personal = match value {
                    Value::String(value) => vec![value.clone()],
                    Value::Array(values) => values
                        .iter()
                        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                        .collect(),
                    _ => Vec::new(),
                };
                if !personal.is_empty() {
                    prefixes = personal;
                }
            }
        }

        prefixes.sort_by_key(|prefix| std::cmp::Reverse(prefix.chars().count()));
        prefixes.dedup();
        if prefixes.is_empty() {
            prefixes.push(".".to_owned());
        }
        prefixes
    }

    pub fn apply_alias(&self, text: &str) -> Option<String> {
        let (command, rest) = match text.split_once(char::is_whitespace) {
            Some((command, rest)) => (command, rest.trim_start()),
            None => (text, ""),
        };
        let aliases = self.db.get_map("CoreMod", "aliases");
        let target = aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(command))
            .and_then(|(_, value)| value.as_str())?;
        Some(if rest.is_empty() {
            target.to_owned()
        } else {
            format!("{target} {rest}")
        })
    }

    pub fn module_enabled(&self, module: &str) -> bool {
        // A module that failed to load is not usable even when the database
        // still lists it as enabled: its handlers and watchers were never
        // registered, so dispatching to it would only produce errors.
        if self.module_failed(module).is_some() {
            return false;
        }
        let disabled = self.db.get_string_list("heroku.main", "disabled_modules");
        !disabled
            .iter()
            .any(|name| name.eq_ignore_ascii_case(module))
    }

    pub fn command_enabled(&self, module: &str, command: &str) -> bool {
        if !self.module_enabled(module) {
            return false;
        }
        let disabled = self.db.get_map("heroku.main", "disabled_commands");
        !disabled
            .get(module)
            .and_then(Value::as_array)
            .map(|commands| {
                commands
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|name| name.eq_ignore_ascii_case(command))
            })
            .unwrap_or(false)
    }

    pub async fn resolve_peer_ref(&self, peer_id: PeerId) -> Option<PeerRef> {
        if peer_id == PeerId::self_user() {
            return Some(PeerId::self_user().to_ambient_ref());
        }
        self.session.peer_ref(peer_id).await.ok().flatten()
    }

    /// Resolve a username, @username or numeric dialog id.
    pub async fn resolve(&self, input: &str) -> Option<PeerRef> {
        let input = input.trim().trim_start_matches('@');
        if input.is_empty() {
            return None;
        }
        if let Ok(id) = input.parse::<i64>() {
            if let Some(peer_id) = PeerId::from_bot_api_dialog_id(id) {
                if let Some(peer_ref) = self.resolve_peer_ref(peer_id).await {
                    return Some(peer_ref);
                }
            }
            return None;
        }
        let peer = self.client.resolve_username(input).await.ok().flatten()?;
        peer.to_ref().await.ok().flatten()
    }

    /// Resolve the target user from the first argument, falling back to a reply.
    pub async fn target_user(&self, message: &Msg, raw: &str) -> Option<PeerRef> {
        let raw = raw.trim().trim_start_matches('@');
        if !raw.is_empty() {
            if let Some(peer) = self.resolve(raw).await {
                return Some(peer);
            }
        }
        let reply = message.get_reply().await.ok().flatten()?;
        let peer_id = reply.inner.sender_id()?;
        self.resolve_peer_ref(peer_id).await
    }

    /// Permission bits of the message sender in the current chat.
    pub async fn chat_rights(&self, message: &Msg) -> Option<u32> {
        if message.outgoing() {
            return Some(ALL);
        }
        let peer_id = message.peer_id();
        if peer_id.kind() == PeerKind::User {
            return Some(crate::module::PM);
        }

        let sender_id = message.inner.sender_id()?;
        if sender_id == PeerId::self_user() {
            return Some(ALL);
        }

        let chat = message.peer_ref().await.ok()?;
        let user = self.resolve_peer_ref(sender_id).await?;
        match self.client.get_permissions(chat, user).await {
            Ok(permissions) => {
                if permissions.is_creator() {
                    Some(GROUP_OWNER | GROUP_ADMIN | GROUP_ADMIN_ANY | GROUP_MEMBER)
                } else if permissions.is_admin() {
                    let mut bits = GROUP_ADMIN | GROUP_ADMIN_ANY | GROUP_MEMBER;
                    if permissions.can_add_admins() {
                        bits |= GROUP_ADMIN_ADD_ADMINS;
                    }
                    Some(bits)
                } else if permissions.is_banned() || permissions.has_left() {
                    None
                } else {
                    Some(GROUP_MEMBER)
                }
            }
            Err(err) => {
                tracing::debug!("get_permissions failed: {err}");
                // Basic chats are notoriously inconsistent; if Telegram says the
                // user is not a participant, do not grant anything.
                None
            }
        }
    }

    pub async fn is_group_admin(&self, message: &Msg) -> bool {
        self.chat_rights(message)
            .await
            .map(|bits| bits & (GROUP_OWNER | GROUP_ADMIN_ANY | GROUP_ADMIN) != 0)
            .unwrap_or(false)
    }

    pub async fn send_self(&self, html: impl Into<String>) -> Result<()> {
        let peer = PeerId::self_user().to_ambient_ref();
        self.client
            .send_message(peer, InputMessage::new().html(html.into()))
            .await?;
        Ok(())
    }

    /// Send a photo card to the owner's saved messages.
    pub async fn send_self_photo(
        &self,
        photo: &str,
        html: impl Into<String>,
        buttons: Vec<Vec<Button>>,
    ) -> Result<()> {
        let peer = PeerId::self_user().to_ambient_ref();
        self.client
            .send_message(
                peer,
                InputMessage::new()
                    .photo_url(photo.to_owned())
                    .html(html.into())
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await?;
        Ok(())
    }

    pub fn user_link(&self, id: i64, name: &str) -> String {
        let fallback = id.to_string();
        format!(
            "<a href=\"tg://user?id={id}\">{}</a>",
            escape_html(if name.trim().is_empty() {
                &fallback
            } else {
                name
            })
        )
    }

    pub fn chat_is_private(message: &Msg) -> bool {
        message.inner.peer_id().kind() == PeerKind::User
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{ModuleLifecycle, ModuleTasks};

    #[test]
    fn failed_module_is_not_loaded_and_reload_clears_the_failure() {
        let mut lifecycle = ModuleLifecycle::default();
        lifecycle.mark_loaded("DemoMod");
        assert!(lifecycle.is_loaded("DemoMod"));
        assert!(lifecycle.failure("DemoMod").is_none());

        lifecycle.mark_failed("DemoMod", "on_load exploded".to_owned());
        assert!(!lifecycle.is_loaded("DemoMod"));
        assert_eq!(
            lifecycle.failure("DemoMod").as_deref(),
            Some("on_load exploded")
        );

        lifecycle.mark_loaded("DemoMod");
        assert!(lifecycle.is_loaded("DemoMod"));
        assert!(lifecycle.failure("DemoMod").is_none());
        assert!(lifecycle.failed_modules().is_empty());
    }

    #[test]
    fn unloading_forgets_a_previous_failure() {
        let mut lifecycle = ModuleLifecycle::default();
        lifecycle.mark_failed("DemoMod", "boom".to_owned());
        lifecycle.mark_unloaded("DemoMod");
        assert!(!lifecycle.is_loaded("DemoMod"));
        assert!(lifecycle.failure("DemoMod").is_none());
    }

    #[tokio::test]
    async fn managed_tasks_are_scoped_per_module_and_aborted() {
        let tasks = ModuleTasks::default();
        let first = std::sync::Arc::new(AtomicUsize::new(0));
        let second = std::sync::Arc::new(AtomicUsize::new(0));

        assert!(tasks.spawn("AlphaMod", {
            let counter = first.clone();
            async move {
                loop {
                    counter.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            }
        }));
        assert!(tasks.spawn("BetaMod", {
            let counter = second.clone();
            async move {
                loop {
                    counter.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
            }
        }));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(tasks.count("AlphaMod"), 1);
        assert_eq!(tasks.count("BetaMod"), 1);

        assert_eq!(tasks.abort("AlphaMod"), 1);
        assert_eq!(tasks.count("AlphaMod"), 0);
        assert_eq!(tasks.count("BetaMod"), 1);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let frozen = first.load(Ordering::Relaxed);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            first.load(Ordering::Relaxed),
            frozen,
            "aborted task must stop running"
        );
        assert!(
            second.load(Ordering::Relaxed) > 0,
            "other module kept running"
        );

        assert_eq!(tasks.abort_all(), 1);
        assert_eq!(tasks.count("BetaMod"), 0);
    }
}
