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

/// `| grep` / `| grep -v` output filter attached to a command message.
#[derive(Clone, Debug, Default)]
pub struct GrepFilter {
    pub include: Option<String>,
    pub exclude: Option<String>,
}

#[derive(Clone)]
pub struct Msg {
    pub inner: Message,
    text_override: Option<String>,
    grep: Option<GrepFilter>,
}

impl Msg {
    pub fn new(inner: Message) -> Self {
        Self {
            inner,
            text_override: None,
            grep: None,
        }
    }

    /// Replace the text returned by [`Msg::text`] (used by aliases and grep).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text_override = Some(text.into());
    }

    /// Attach a `| grep` filter to every text reply/edit produced by the
    /// command handler.
    pub fn set_grep(&mut self, filter: GrepFilter) {
        self.grep = Some(filter);
    }

    fn apply_grep(&self, html: &str) -> String {
        match &self.grep {
            Some(filter) => Self::grep_filter_text(html, filter),
            None => html.to_owned(),
        }
    }

    fn grep_filter_text(html: &str, filter: &GrepFilter) -> String {
        let mut matched = Vec::new();
        for line in html.split('\n') {
            let plain = strip_html(line);
            let included = filter
                .include
                .as_ref()
                .map(|pattern| plain.contains(pattern))
                .unwrap_or(true);
            let excluded = filter
                .exclude
                .as_ref()
                .is_some_and(|pattern| plain.contains(pattern));
            if included && !excluded {
                let mut escaped = escape_html(&plain);
                if let Some(pattern) = &filter.include {
                    let escaped_pattern = escape_html(pattern);
                    escaped =
                        escaped.replace(&escaped_pattern, &format!("<u>{escaped_pattern}</u>"));
                }
                matched.push(escaped);
            }
        }

        let mut condition = String::new();
        if let Some(pattern) = &filter.include {
            condition.push_str(&format!("contain <b>{}</b>", escape_html(pattern)));
        }
        if let Some(pattern) = &filter.exclude {
            if filter.include.is_some() {
                condition.push_str(" and ");
            }
            condition.push_str(&format!("do not contain <b>{}</b>", escape_html(pattern)));
        }

        if matched.is_empty() {
            format!("💬 <i>No lines that {condition}</i>")
        } else {
            format!("<i>💬 Lines that {condition}:</i>\n{}", matched.join("\n"))
        }
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

    /// Broadcast channel (not a megagroup). Used to ignore edits, matching the
    /// Python dispatcher.
    pub fn is_broadcast_channel(&self) -> bool {
        matches!(self.peer(), Some(Peer::Channel(_)))
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
        self.inner
            .sender_ref()
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
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
        crate::protection::guard().await;
        let message = InputMessage::new().html(self.apply_grep(&html.into()));
        Ok(Msg::new(self.inner.reply(message).await?))
    }

    pub async fn respond_html(&self, html: impl Into<String>) -> Result<Msg> {
        crate::protection::guard().await;
        let message = InputMessage::new().html(self.apply_grep(&html.into()));
        Ok(Msg::new(self.inner.respond(message).await?))
    }

    pub async fn respond_file(
        &self,
        client: &Client,
        path: &Path,
        caption: impl Into<String>,
    ) -> Result<Msg> {
        crate::protection::guard().await;
        let uploaded = client
            .upload_file(path)
            .await
            .with_context(|| format!("failed to upload {}", path.display()))?;
        let message = InputMessage::new().html(caption.into()).file(uploaded);
        Ok(Msg::new(self.inner.respond(message).await?))
    }

    pub async fn edit_html(&self, html: impl Into<String>) -> Result<()> {
        crate::protection::guard().await;
        self.inner
            .edit(InputMessage::new().html(self.apply_grep(&html.into())))
            .await?;
        Ok(())
    }

    pub async fn delete(&self) -> Result<()> {
        crate::protection::guard().await;
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

/// Minimal HTML unescape used by the `| grep` line filter.
pub fn unescape_html(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Strip Telegram HTML tags and decode entities so `| grep` matches the text
/// the user actually sees.
pub fn strip_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    unescape_html(&out)
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

#[cfg(test)]
mod grep_tests {
    use super::{GrepFilter, Msg};

    #[test]
    fn filters_and_underlines_matches() {
        let filter = GrepFilter {
            include: Some("needle".to_owned()),
            exclude: None,
        };
        let out = Msg::grep_filter_text("<b>hay</b>\nneedle here\nbye", &filter);
        assert!(out.contains("<u>needle</u> here"));
        assert!(!out.contains("hay"));
        assert!(!out.contains("bye"));
    }

    #[test]
    fn exclude_only_keeps_other_lines() {
        let filter = GrepFilter {
            include: None,
            exclude: Some("owner".to_owned()),
        };
        let out = Msg::grep_filter_text("mask for owner\nmask for admin", &filter);
        assert!(!out.contains("mask for owner"));
        assert!(out.contains("mask for admin"));
        assert!(out.contains("do not contain <b>owner</b>"));
    }
}
