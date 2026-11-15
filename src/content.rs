// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Content channel and forum topics, mirroring Python's `Quickstart` setup.
//!
//! Heroku keeps assets and backups in a private channel named `heroku-userbot`
//! with two forum topics: `Assets` and `Backups`. The channel is created on the
//! first launch and reused afterwards. The inline bot is invited to the channel
//! so it can send backup files there.

use std::sync::Arc;

use anyhow::{Context, Result};
use grammers_client::message::InputMessage;
use grammers_client::peer::Peer;
use grammers_session::types::{ChannelKind, PeerAuth, PeerId, PeerInfo, PeerRef};
use grammers_tl_types as tl;
use tracing::{debug, warn};

use crate::ctx::Ctx;

pub const CHANNEL_TITLE: &str = "heroku-userbot";
pub const CHANNEL_ABOUT: &str = "🪐 Content related to Heroku will be here";

pub const TOPIC_ASSETS: &str = "Assets";
pub const TOPIC_ASSETS_DESCRIPTION: &str = "🌆 Your Heroku assets will be stored here";
pub const TOPIC_BACKUPS: &str = "Backups";
pub const TOPIC_BACKUPS_DESCRIPTION: &str = "💾 Your Heroku backups will be stored here";

/// Return the content channel reference, creating the channel if needed.
pub async fn ensure_content_channel(ctx: &Arc<Ctx>) -> Result<PeerRef> {
    // 1. Try the channel saved in the database.
    let saved = ctx.db.get_i64("heroku.forums", "channel_id", 0);
    let mut channel = if saved != 0 {
        channel_ref(ctx, saved).await
    } else {
        None
    };

    // 2. Look for an existing `heroku-userbot` channel in the dialog list.
    if channel.is_none() {
        if let Some((found, dialog_ref)) = find_channel_dialog(ctx).await {
            debug!("Found existing content channel {found}");
            ctx.db.set("heroku.forums", "channel_id", found);
            channel = Some(dialog_ref);
        }
    }

    // 3. Create a new private forum channel.
    let channel = match channel {
        Some(channel) => channel,
        None => {
            let channel = create_content_channel(ctx).await?;
            ctx.db.set(
                "heroku.forums",
                "channel_id",
                channel.id.bare_id_unchecked(),
            );
            debug!("Created content channel {}", channel.id);
            channel
        }
    };

    ctx.db
        .set("heroku.forums", "forum_id", channel.id.bare_id_unchecked());

    // Channels created by older versions may not be forums yet. Ignore errors:
    // Telegram rejects the request if the channel already has forum enabled.
    let input: tl::enums::InputChannel = (&channel).into();
    let _ = ctx
        .client
        .invoke(&tl::functions::channels::ToggleForum {
            channel: input,
            enabled: true,
            tabs: false,
            sidebar: false,
        })
        .await;

    Ok(channel)
}

/// Ensure the content channel and its built-in topics exist.
pub async fn ensure_topics(ctx: &Arc<Ctx>) -> Result<()> {
    let channel = ensure_content_channel(ctx).await?;

    for (title, description) in [
        (TOPIC_ASSETS, TOPIC_ASSETS_DESCRIPTION),
        (TOPIC_BACKUPS, TOPIC_BACKUPS_DESCRIPTION),
    ] {
        match ensure_topic_in(&channel, ctx, title, description).await {
            Ok(id) => debug!("Content topic '{title}' is ready (id {id})"),
            Err(err) => warn!("Failed to create topic '{title}': {err:#}"),
        }
    }

    if let Err(err) = invite_inline_bot(ctx, &channel).await {
        warn!("Failed to invite the inline bot to the content channel: {err:#}");
    }

    Ok(())
}

/// Find the id of a forum topic, using the database cache first and querying
/// Telegram afterwards. Returns `None` if the topic does not exist.
pub async fn topic_id(ctx: &Ctx, title: &str) -> Option<i32> {
    if let Some(id) = cached_topic(ctx, title) {
        return Some(id);
    }

    let channel = channel_from_db(ctx).await?;
    match fetch_topics(ctx, &channel).await {
        Ok(topics) => {
            let found = topics
                .into_iter()
                .find(|topic| topic.title == title)
                .map(|topic| topic.id);
            if let Some(id) = found {
                cache_topic(ctx, title, id);
            }
            found
        }
        Err(err) => {
            debug!("Failed to fetch forum topics: {err:#}");
            None
        }
    }
}

/// Ensure a single forum topic exists and return its id.
pub async fn ensure_topic(ctx: &Arc<Ctx>, title: &str, description: &str) -> Result<i32> {
    let channel = ensure_content_channel(ctx).await?;
    ensure_topic_in(&channel, ctx, title, description).await
}

/// Send a message into a forum topic, creating the topic when necessary.
pub async fn send_to_topic(
    ctx: &Arc<Ctx>,
    title: &str,
    description: &str,
    message: InputMessage,
) -> Result<()> {
    let channel = ensure_content_channel(ctx).await?;
    let topic = ensure_topic_in(&channel, ctx, title, description).await?;
    ctx.client
        .send_message(channel, message.reply_to(Some(topic)))
        .await
        .context("failed to send a message to the content channel")?;
    Ok(())
}

async fn ensure_topic_in(
    channel: &PeerRef,
    ctx: &Arc<Ctx>,
    title: &str,
    description: &str,
) -> Result<i32> {
    let topics = fetch_topics(ctx, channel).await.unwrap_or_default();
    if let Some(topic) = topics.iter().find(|topic| topic.title == title) {
        cache_topic(ctx, title, topic.id);
        return Ok(topic.id);
    }

    let input: tl::enums::InputPeer = channel.into();
    let updates = ctx
        .client
        .invoke(&tl::functions::messages::CreateForumTopic {
            title_missing: false,
            peer: input,
            title: title.to_owned(),
            icon_color: None,
            icon_emoji_id: None,
            random_id: rand::random::<i64>(),
            send_as: None,
        })
        .await
        .with_context(|| format!("failed to create forum topic '{title}'"))?;

    let topic_id = first_message_id(&updates)
        .with_context(|| format!("Telegram did not return a topic id for '{title}'"))?;

    let _ = ctx
        .client
        .send_message(
            *channel,
            InputMessage::new()
                .html(description)
                .reply_to(Some(topic_id)),
        )
        .await;

    cache_topic(ctx, title, topic_id);
    Ok(topic_id)
}

async fn fetch_topics(ctx: &Ctx, channel: &PeerRef) -> Result<Vec<tl::types::ForumTopic>> {
    let input: tl::enums::InputPeer = channel.into();
    let result = ctx
        .client
        .invoke(&tl::functions::messages::GetForumTopics {
            peer: input,
            q: None,
            offset_date: 0,
            offset_id: 0,
            offset_topic: 0,
            limit: 100,
        })
        .await
        .context("messages.getForumTopics failed")?;

    let tl::enums::messages::ForumTopics::Topics(result) = result;
    Ok(result
        .topics
        .into_iter()
        .filter_map(|topic| match topic {
            tl::enums::ForumTopic::Topic(topic) => Some(topic),
            tl::enums::ForumTopic::Deleted(_) => None,
        })
        .collect())
}

async fn create_content_channel(ctx: &Arc<Ctx>) -> Result<PeerRef> {
    let updates = ctx
        .client
        .invoke(&tl::functions::channels::CreateChannel {
            broadcast: true,
            megagroup: false,
            for_import: false,
            forum: true,
            title: CHANNEL_TITLE.to_owned(),
            about: CHANNEL_ABOUT.to_owned(),
            geo_point: None,
            address: None,
            ttl_period: None,
        })
        .await
        .context("channels.createChannel failed")?;

    let channel = channels_in_updates(&updates)
        .into_iter()
        .find(|channel| channel.title == CHANNEL_TITLE)
        .or_else(|| channels_in_updates(&updates).into_iter().next())
        .context("Telegram did not return the created channel")?;

    let access_hash = channel
        .access_hash
        .context("the created channel has no access hash")?;
    let reference = PeerRef {
        id: PeerId::channel_unchecked(channel.id),
        auth: PeerAuth::from_hash(access_hash),
    };
    let _ = ctx
        .session
        .cache_peer(PeerInfo::Channel {
            id: channel.id,
            auth: Some(PeerAuth::from_hash(access_hash)),
            kind: Some(ChannelKind::Broadcast),
        })
        .await;

    Ok(reference)
}

async fn find_channel_dialog(ctx: &Arc<Ctx>) -> Option<(i64, PeerRef)> {
    let mut dialogs = ctx.client.iter_dialogs();
    while let Ok(Some(dialog)) = dialogs.next().await {
        if !matches!(dialog.peer, Peer::Channel(_)) {
            continue;
        }
        let Some(name) = dialog.peer.name() else {
            continue;
        };
        if name.eq_ignore_ascii_case(CHANNEL_TITLE) {
            return Some((dialog.peer.id().bare_id_unchecked(), dialog.peer_ref()));
        }
    }
    None
}

async fn channel_ref(ctx: &Ctx, id: i64) -> Option<PeerRef> {
    let peer_id = PeerId::channel(id)?;
    match ctx.session.peer_ref(peer_id).await {
        Ok(Some(reference)) => Some(reference),
        Ok(None) => None,
        Err(err) => {
            debug!("Failed to read the content channel from the session: {err}");
            None
        }
    }
}

async fn channel_from_db(ctx: &Ctx) -> Option<PeerRef> {
    let id = ctx.db.get_i64("heroku.forums", "channel_id", 0);
    if id == 0 {
        return None;
    }
    channel_ref(ctx, id).await
}

fn cached_topic(ctx: &Ctx, title: &str) -> Option<i32> {
    let cache = ctx.db.get_map("heroku.forums", "forums_cache");
    cache
        .get(CHANNEL_TITLE)
        .and_then(|value| value.get(title))
        .and_then(|value| value.as_i64())
        .and_then(|value| i32::try_from(value).ok())
}

fn cache_topic(ctx: &Ctx, title: &str, id: i32) {
    let mut cache = ctx.db.get_map("heroku.forums", "forums_cache");
    let entry = cache
        .entry(CHANNEL_TITLE.to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(object) = entry.as_object_mut() {
        object.insert(title.to_owned(), serde_json::Value::from(id));
    }
    ctx.db.set(
        "heroku.forums",
        "forums_cache",
        serde_json::Value::Object(cache),
    );
}

async fn invite_inline_bot(ctx: &Arc<Ctx>, channel: &PeerRef) -> Result<()> {
    let Some(username) = ctx.inline.bot_username().await else {
        return Ok(());
    };
    let Some(peer) = ctx
        .client
        .resolve_username(&username)
        .await
        .context("failed to resolve the inline bot")?
    else {
        return Ok(());
    };
    let Some(bot) = peer
        .to_ref()
        .await
        .map_err(|err| anyhow::anyhow!("failed to resolve the bot ref: {err}"))?
    else {
        return Ok(());
    };

    let input: tl::enums::InputChannel = channel.into();
    let user: tl::enums::InputUser = (&bot).into();
    match ctx
        .client
        .invoke(&tl::functions::channels::InviteToChannel {
            channel: input.clone(),
            users: vec![user.clone()],
        })
        .await
    {
        Ok(_) => debug!("Invited @{username} to the content channel"),
        // USER_ALREADY_PARTICIPANT is expected after the first launch.
        Err(err) => debug!("Inline bot invite skipped: {err}"),
    }

    let rights = tl::types::ChatAdminRights {
        change_info: false,
        post_messages: false,
        edit_messages: false,
        delete_messages: false,
        ban_users: true,
        invite_users: false,
        pin_messages: false,
        add_admins: false,
        anonymous: false,
        manage_call: false,
        other: false,
        manage_topics: false,
        post_stories: false,
        edit_stories: false,
        delete_stories: false,
        manage_direct_messages: false,
        manage_ranks: false,
        manage_linked_peers: false,
        manage_welcome_messages: false,
    };
    let _ = ctx
        .client
        .invoke(&tl::functions::channels::EditAdmin {
            channel: input,
            user_id: user,
            admin_rights: tl::enums::ChatAdminRights::Rights(rights),
            rank: Some("Heroku".to_owned()),
        })
        .await;

    Ok(())
}

fn channels_in_updates(updates: &tl::enums::Updates) -> Vec<&tl::types::Channel> {
    let chats = match updates {
        tl::enums::Updates::Updates(updates) => &updates.chats,
        tl::enums::Updates::Combined(updates) => &updates.chats,
        _ => return Vec::new(),
    };
    chats
        .iter()
        .filter_map(|chat| match chat {
            tl::enums::Chat::Channel(channel) => Some(channel),
            _ => None,
        })
        .collect()
}

fn first_message_id(updates: &tl::enums::Updates) -> Option<i32> {
    let updates = match updates {
        tl::enums::Updates::Updates(updates) => &updates.updates,
        tl::enums::Updates::Combined(updates) => &updates.updates,
        _ => return None,
    };
    updates.iter().find_map(|update| match update {
        tl::enums::Update::NewChannelMessage(message) => Some(message.message.id()),
        tl::enums::Update::NewMessage(message) => Some(message.message.id()),
        _ => None,
    })
}
