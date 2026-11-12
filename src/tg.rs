// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Convenience wrapper around grammers messages.

use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result};
use grammers_client::message::{InputMessage, Message};
use grammers_client::peer::Peer;
use grammers_client::Client;
use grammers_session::types::{PeerId, PeerKind, PeerRef};

static SELF_ID: AtomicI64 = AtomicI64::new(0);

pub fn set_self_id(id: i64) {
    SELF_ID.store(id, Ordering::Relaxed);
}

pub fn self_id() -> i64 {
    SELF_ID.load(Ordering::Relaxed)
}

fn dialog_id(peer: PeerId) -> i64 {
    if peer == PeerId::self_user() {
        self_id()
    } else {
        peer.bot_api_dialog_id_unchecked()
    }
}

#[derive(Clone)]
pub struct Msg {
    pub inner: Message,
    text_override: Option<String>,
}

impl Msg {
    pub fn new(inner: Message) -> Self {
        Self {
            inner,
            text_override: None,
        }
    }

    /// Replace the text returned by [`Msg::text`] (used by aliases and grep).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text_override = Some(text.into());
    }

    pub fn id(&self) -> i32 {
        self.inner.id()
    }

    pub fn text(&self) -> &str {
        self.text_override
            .as_deref()
            .unwrap_or_else(|| self.inner.text())
    }

    /// Telegram-style dialog id (users positive, chats negative, channels `-100…`).
    pub fn chat_id(&self) -> i64 {
        dialog_id(self.inner.peer_id())
    }

    pub fn peer_id(&self) -> PeerId {
        self.inner.peer_id()
    }

    pub fn sender_id(&self) -> Option<i64> {
        self.inner.sender_id().map(dialog_id)
    }

    pub fn outgoing(&self) -> bool {
        self.inner.outgoing()
    }

    pub fn mentioned(&self) -> bool {
        self.inner.mentioned()
    }

    pub fn is_private(&self) -> bool {
        self.inner.peer_id().kind() == PeerKind::User
    }

    pub fn is_channel(&self) -> bool {
        self.inner.peer_id().kind() == PeerKind::Channel
    }

    pub fn reply_to_id(&self) -> Option<i32> {
        self.inner.reply_to_message_id()
    }

    pub async fn peer_ref(&self) -> Result<PeerRef> {
        let peer = self
            .inner
            .peer_ref()
            .await
            .map_err(|err| anyhow::anyhow!("message peer is not in cache: {err}"))?;
        peer.ok_or_else(|| anyhow::anyhow!("message peer has no access hash"))
    }

    pub async fn sender_ref(&self) -> Result<Option<PeerRef>> {
        Ok(self
            .inner
            .sender_ref()
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?)
    }

    pub fn peer(&self) -> Option<&Peer> {
        self.inner.peer()
    }

    pub fn sender(&self) -> Option<&Peer> {
        self.inner.sender()
    }

    pub fn sender_display(&self) -> String {
        self.sender()
            .map(peer_display)
            .or_else(|| self.peer().map(peer_display))
            .unwrap_or_else(|| "unknown".to_owned())
    }

    pub fn chat_display(&self) -> String {
        self.peer()
            .map(peer_display)
            .unwrap_or_else(|| self.chat_id().to_string())
    }

    pub async fn reply_html(&self, html: impl Into<String>) -> Result<Msg> {
        let message = InputMessage::new().html(html.into());
        Ok(Msg::new(self.inner.reply(message).await?))
    }

    pub async fn respond_html(&self, html: impl Into<String>) -> Result<Msg> {
        let message = InputMessage::new().html(html.into());
        Ok(Msg::new(self.inner.respond(message).await?))
    }

    pub async fn respond_file(
        &self,
        client: &Client,
        path: &Path,
        caption: impl Into<String>,
    ) -> Result<Msg> {
        let uploaded = client
            .upload_file(path)
            .await
            .with_context(|| format!("failed to upload {}", path.display()))?;
        let message = InputMessage::new().html(caption.into()).file(uploaded);
        Ok(Msg::new(self.inner.respond(message).await?))
    }

    pub async fn edit_html(&self, html: impl Into<String>) -> Result<()> {
        self.inner
            .edit(InputMessage::new().html(html.into()))
            .await?;
        Ok(())
    }

    pub async fn delete(&self) -> Result<()> {
        self.inner.delete().await?;
        Ok(())
    }

    pub async fn get_reply(&self) -> Result<Option<Msg>> {
        Ok(self.inner.get_reply().await?.map(Msg::new))
    }

    pub async fn download_media(&self, path: &Path) -> Result<()> {
        self.inner
            .download_media(path)
            .await
            .with_context(|| format!("failed to download media to {}", path.display()))?;
        Ok(())
    }

    pub fn html_text(&self) -> String {
        escape_html(self.text())
    }
}

pub fn peer_display(peer: &Peer) -> String {
    match peer {
        Peer::User(user) => {
            let name = user.full_name();
            if name.trim().is_empty() {
                user.username()
                    .map(|username| format!("@{username}"))
                    .unwrap_or_else(|| user.id().bot_api_dialog_id_unchecked().to_string())
            } else {
                name
            }
        }
        Peer::Group(group) => group.title().unwrap_or("group").to_owned(),
        Peer::Channel(channel) => channel.title().to_owned(),
        Peer::Community(community) => community.title().to_owned(),
    }
}

pub fn user_link(peer: &Peer) -> String {
    match peer {
        Peer::User(user) => format!(
            "<a href=\"tg://user?id={}\">{}</a>",
            user.id().bot_api_dialog_id_unchecked(),
            escape_html(&peer_display(peer))
        ),
        _ => escape_html(&peer_display(peer)),
    }
}

pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

/// Split long text into Telegram-safe chunks without breaking HTML tags mid-way
/// whenever possible. Entities are not preserved for cut points; callers that
/// need rich formatting should keep messages short.
pub fn chunks(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_owned()];
    }
    let mut result = Vec::new();
    let mut current = String::new();
    for line in text.split_inclusive('\n') {
        if current.chars().count() + line.chars().count() > limit {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
            if line.chars().count() > limit {
                for part in line.as_bytes().chunks(limit) {
                    result.push(String::from_utf8_lossy(part).into_owned());
                }
                continue;
            }
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}
