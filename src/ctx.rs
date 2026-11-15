// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Shared runtime context available to every module.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use grammers_client::message::InputMessage;
use grammers_client::Client;
use grammers_session::storages::ErasedSession;
use grammers_session::types::{PeerId, PeerKind, PeerRef};
use serde_json::Value;

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
}

impl Ctx {
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
