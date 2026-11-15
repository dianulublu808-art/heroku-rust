// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Small helpers shared by built-in modules.

use std::sync::Arc;

use anyhow::Result;
use grammers_session::types::PeerRef;

use crate::ctx::Ctx;
use crate::tg::{escape_html, Msg};

pub async fn answer(message: &Msg, html: impl Into<String>) -> Result<()> {
    message.reply_html(html).await?;
    Ok(())
}

pub fn peer_id(peer: &PeerRef) -> i64 {
    peer.id.bot_api_dialog_id_unchecked()
}

/// Resolve the target user from a reply or from the first argument.
pub async fn target_user(ctx: &Arc<Ctx>, message: &Msg, raw: &str) -> Option<PeerRef> {
    ctx.target_user(message, raw).await
}

/// Resolve the target chat from an argument or use the current chat.
pub async fn target_chat(ctx: &Arc<Ctx>, message: &Msg, raw: &str) -> Option<PeerRef> {
    if raw.trim().is_empty() {
        return message.peer_ref().await.ok();
    }
    ctx.resolve(raw).await
}

/// Normalize a chat id the way Python's `utils.get_chat_id` does: channels are
/// stored without the `-100` prefix.
pub fn blacklist_chat_id(chat_id: i64) -> i64 {
    if chat_id <= -1_000_000_000_000 {
        chat_id + 1_000_000_000_000
    } else {
        chat_id
    }
}

pub fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enable" | "enabled" | "+" => Some(true),
        "0" | "false" | "no" | "off" | "disable" | "disabled" | "-" => Some(false),
        _ => None,
    }
}

/// Render a value for `.config` and the settings list.
pub fn show_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => format!("<code>{}</code>", escape_html(text)),
        serde_json::Value::Null => "<code>None</code>".to_owned(),
        other => format!("<code>{}</code>", escape_html(&other.to_string())),
    }
}

pub fn module_config_key(module: &str, key: &str) -> (String, String) {
    (module.to_owned(), key.to_owned())
}

pub fn split_once(text: &str) -> (&str, &str) {
    match text.find(char::is_whitespace) {
        Some(index) => (&text[..index], text[index..].trim_start()),
        None => (text, ""),
    }
}
