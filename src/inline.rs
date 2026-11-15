// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Real inline-bot runtime: a second Telegram connection signed in with a bot
//! token, an update loop for inline queries/callbacks/bot PM and the result
//! builders used by `.inline` and `@bot`.
//!
//! The userbot itself cannot receive inline queries, so — exactly like the
//! Python version — we keep a separate bot client in `sessions/inline-*.session`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use grammers_client::message::{Button, InputMessage, Message, ReplyMarkup};
use grammers_client::update::{Article, CallbackQuery, InlineQuery, Update};
use grammers_client::Client;
use grammers_mtsender::{SenderPool, SenderPoolFatHandle, UpdatesConfiguration};
use grammers_session::storages::SqliteSession;
use grammers_session::types::{PeerId, PeerRef};
use grammers_tl_types as tl;
use rand::Rng;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigKind};
use crate::security::NS as SECURITY_NS;
use crate::tg::escape_html;

pub const INLINE_NS: &str = "heroku.inline";
const DEFAULT_THUMB: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/start_cmd.jpg";
const SUPPORT_URL: &str = "https://t.me/heroku_talks";
const GITHUB_URL: &str = "https://github.com/Heroku-userbot/Heroku";

#[derive(Debug, Clone, Default)]
pub struct InlineStatus {
    pub configured: bool,
    pub running: bool,
    pub username: Option<String>,
    pub bot_id: Option<i64>,
    pub error: Option<String>,
}

struct BotRuntime {
    client: Client,
    bot_id: i64,
    username: String,
    handle: SenderPoolFatHandle,
    pool_task: JoinHandle<()>,
    updates_task: JoinHandle<()>,
}

/// A value the inline bot asked the owner to type as the next private message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInput {
    AccountPhone,
    AccountCode,
    AccountPassword,
    ConfigValue {
        module: &'static str,
        key: &'static str,
    },
    SecurityGroupUser {
        group: String,
    },
    SecurityGroupRule {
        group: String,
    },
    OwnerAdd,
    GroupCreate,
    SudoPassword {
        chat_id: i64,
        message_id: i32,
    },
}

/// Owns the optional bot connection and all inline state.
pub struct InlineManager {
    runtime: Mutex<Option<BotRuntime>>,
    status: StdMutex<InlineStatus>,
    started: StdMutex<Option<Instant>>,
    restarting: AtomicBool,
    pending: StdMutex<HashMap<i64, PendingInput>>,
}

impl Default for InlineManager {
    fn default() -> Self {
        Self::new()
    }
}

impl InlineManager {
    pub fn new() -> Self {
        Self {
            runtime: Mutex::new(None),
            status: StdMutex::new(InlineStatus::default()),
            started: StdMutex::new(None),
            restarting: AtomicBool::new(false),
            pending: StdMutex::new(HashMap::new()),
        }
    }

    pub fn set_pending(&self, user_id: i64, input: PendingInput) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.insert(user_id, input);
        }
    }

    pub fn take_pending(&self, user_id: i64) -> Option<PendingInput> {
        self.pending.lock().ok()?.remove(&user_id)
    }

    pub fn clear_pending(&self, user_id: i64) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&user_id);
        }
    }

    /// Token stored in the database, falling back to the config/env value.
    pub fn stored_token(ctx: &Ctx) -> Option<String> {
        let from_db = ctx.db.get_str(INLINE_NS, "bot_token", "");
        let token = if from_db.trim().is_empty() {
            ctx.config.inline_bot_token.clone().unwrap_or_default()
        } else {
            from_db
        };
        let token = token.trim().to_owned();
        (!token.is_empty()).then_some(token)
    }

    pub async fn snapshot(&self) -> InlineStatus {
        let mut status = self
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default();
        let runtime = self.runtime.lock().await;
        status.running = runtime.is_some();
        status
    }

    pub async fn bot_id(&self) -> Option<i64> {
        self.runtime
            .lock()
            .await
            .as_ref()
            .map(|runtime| runtime.bot_id)
    }

    pub async fn bot_username(&self) -> Option<String> {
        self.runtime
            .lock()
            .await
            .as_ref()
            .map(|runtime| runtime.username.clone())
    }

    /// Send an arbitrary card from the inline bot to a user, if the bot is
    /// running and the user has started it.
    pub async fn send_to_user(
        &self,
        ctx: &Arc<Ctx>,
        user_id: i64,
        html: String,
        buttons: Vec<Vec<Button>>,
    ) -> Result<()> {
        let _ = ctx;
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            anyhow::bail!("inline bot is not running");
        };
        let peer = PeerId::from_bot_api_dialog_id(user_id)
            .context("invalid Telegram user id")?
            .to_ambient_ref();
        runtime
            .client
            .send_message(
                peer,
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to send an inline card: {err}"))?;
        Ok(())
    }

    /// Send a quiz poll from the inline bot and return `(message_id, poll_id)`.
    /// The poll id is assigned by Telegram, so it is read back from the sent
    /// message; `LoaderRestrictor` needs it to match incoming votes.
    pub async fn send_quiz_poll(
        &self,
        user_id: i64,
        question: &str,
        answers: &[String],
        correct: i32,
    ) -> Result<(i32, i64)> {
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            anyhow::bail!("inline bot is not running");
        };
        let peer = PeerId::from_bot_api_dialog_id(user_id)
            .context("invalid Telegram user id")?
            .to_ambient_ref();
        let input: tl::enums::InputPeer = (&peer).into();

        let parse = |text: &str| {
            let (plain, entities) = grammers_client::parsers::parse_html_message(text);
            tl::enums::TextWithEntities::Entities(tl::types::TextWithEntities {
                text: plain,
                entities,
            })
        };

        let poll = tl::types::Poll {
            id: 0,
            closed: false,
            public_voters: true,
            multiple_choice: false,
            quiz: true,
            open_answers: false,
            revoting_disabled: true,
            shuffle_answers: false,
            hide_results_until_close: false,
            creator: false,
            subscribers_only: false,
            question: parse(question),
            answers: answers
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    tl::enums::PollAnswer::Answer(tl::types::PollAnswer {
                        text: parse(text),
                        option: index.to_string().into_bytes(),
                        media: None,
                        added_by: None,
                        date: None,
                    })
                })
                .collect(),
            close_period: Some(60),
            close_date: None,
            countries_iso2: None,
            hash: 0,
        };

        let media = tl::enums::InputMedia::Poll(Box::new(tl::types::InputMediaPoll {
            poll: tl::enums::Poll::Poll(poll),
            correct_answers: Some(vec![correct]),
            attached_media: None,
            solution: None,
            solution_entities: None,
            solution_media: None,
        }));

        let random_id: i64 = rand::thread_rng().gen();
        let response = runtime
            .client
            .invoke(&tl::functions::messages::SendMedia {
                silent: true,
                background: false,
                clear_draft: false,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                allow_paid_floodskip: false,
                peer: input,
                reply_to: None,
                media,
                message: String::new(),
                random_id,
                reply_markup: None,
                entities: None,
                schedule_date: None,
                schedule_repeat_period: None,
                send_as: None,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
            .map_err(|err| anyhow::anyhow!("failed to send a quiz poll: {err}"))?;

        let mut message_id = None;
        let mut poll_id = None;
        if let tl::enums::Updates::Updates(updates) = response {
            for update in updates.updates {
                if let tl::enums::Update::NewMessage(raw) = update {
                    if let tl::enums::Message::Message(message) = raw.message {
                        message_id.get_or_insert(message.id);
                        if let Some(tl::enums::MessageMedia::Poll(poll)) = message.media {
                            let tl::enums::Poll::Poll(inner) = poll.poll;
                            poll_id = Some(inner.id);
                        }
                    }
                }
            }
        }

        match (message_id, poll_id) {
            (Some(message_id), Some(poll_id)) => Ok((message_id, poll_id)),
            _ => anyhow::bail!("Telegram did not return the quiz poll identifier"),
        }
    }

    /// Delete messages previously sent by the inline bot to `user_id`.
    pub async fn delete_bot_messages(&self, user_id: i64, ids: &[i32]) {
        if ids.is_empty() {
            return;
        }
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            return;
        };
        let Some(peer) = PeerId::from_bot_api_dialog_id(user_id) else {
            return;
        };
        let _ = runtime
            .client
            .delete_messages(peer.to_ambient_ref(), ids)
            .await;
    }

    /// Send a photo card from the inline bot to a user. `photo` may be an
    /// HTTP(S) URL, matching Python's `bot.send_photo(...)`.
    pub async fn send_photo_to_user(
        &self,
        ctx: &Arc<Ctx>,
        user_id: i64,
        photo: &str,
        html: String,
        buttons: Vec<Vec<Button>>,
    ) -> Result<()> {
        let _ = ctx;
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            anyhow::bail!("inline bot is not running");
        };
        let peer = PeerId::from_bot_api_dialog_id(user_id)
            .context("invalid Telegram user id")?
            .to_ambient_ref();
        runtime
            .client
            .send_message(
                peer,
                InputMessage::new()
                    .photo_url(photo.to_owned())
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to send an inline photo card: {err}"))?;
        Ok(())
    }

    /// Send a document from the inline bot into a forum topic of the content
    /// channel. Returns the message id. Mirrors Python's
    /// `inline.bot.send_document(..., message_thread_id=...)`.
    pub async fn send_document_to_channel(
        &self,
        ctx: &Arc<Ctx>,
        channel_id: i64,
        topic_id: i32,
        path: &Path,
        caption: String,
        buttons: &[Vec<Button>],
    ) -> Result<i32> {
        let _ = ctx;
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            anyhow::bail!("inline bot is not running");
        };

        let mut dialogs = runtime.client.iter_dialogs();
        let mut channel = None;
        while let Ok(Some(dialog)) = dialogs.next().await {
            if dialog.peer.id().bare_id_unchecked() == channel_id {
                channel = Some(dialog.peer_ref());
                break;
            }
        }
        let channel = channel.context("the inline bot is not a member of the content channel")?;

        let uploaded = runtime
            .client
            .upload_file(path)
            .await
            .with_context(|| format!("failed to upload {}", path.display()))?;
        let sent = runtime
            .client
            .send_message(
                channel,
                InputMessage::new()
                    .html(caption)
                    .document(uploaded)
                    .reply_to(Some(topic_id))
                    .reply_markup(ReplyMarkup::from_buttons(buttons)),
            )
            .await
            .context("failed to send the file from the inline bot")?;
        Ok(sent.id())
    }

    /// Send the configuration module picker from the inline bot.
    pub async fn send_config_page(&self, ctx: &Arc<Ctx>, user_id: i64) -> Result<()> {
        let (html, buttons) = config_list_card(ctx);
        self.send_to_user(ctx, user_id, html, buttons).await
    }

    /// Send the configuration card of a single module.
    pub async fn send_config_module(
        &self,
        ctx: &Arc<Ctx>,
        user_id: i64,
        module_name: &str,
    ) -> Result<()> {
        let index = ctx
            .modules
            .all()
            .iter()
            .position(|module| module.name().eq_ignore_ascii_case(module_name))
            .context("module not found")?;
        let (html, buttons) =
            config_module_card(ctx, index).context("this module has no configuration")?;
        self.send_to_user(ctx, user_id, html, buttons).await
    }

    /// Send the interactive help cover with paginated module buttons.
    pub async fn send_help_page(&self, ctx: &Arc<Ctx>, user_id: i64, page: usize) -> Result<()> {
        let (html, buttons) = help_page_card(ctx, page);
        self.send_to_user(ctx, user_id, html, buttons).await
    }

    /// Send the help card of a single module.
    pub async fn send_help_module(
        &self,
        ctx: &Arc<Ctx>,
        user_id: i64,
        module_name: &str,
    ) -> Result<()> {
        let index = ctx
            .modules
            .all()
            .iter()
            .position(|module| module.name().eq_ignore_ascii_case(module_name))
            .context("module not found")?;
        let (html, buttons) = help_module_card(ctx, index).context("unknown module")?;
        self.send_to_user(ctx, user_id, html, buttons).await
    }

    /// Send the first-run welcome card from the inline bot, with the language
    /// picker used by the Python `Quickstart` module.
    pub async fn send_welcome(&self, ctx: &Arc<Ctx>, user_id: i64) -> Result<()> {
        let runtime = self.runtime.lock().await;
        let Some(runtime) = runtime.as_ref() else {
            anyhow::bail!("inline bot is not running");
        };
        let peer = PeerId::from_bot_api_dialog_id(user_id)
            .context("invalid Telegram user id")?
            .to_ambient_ref();
        runtime
            .client
            .send_message(
                peer,
                InputMessage::new()
                    .html(welcome_text(ctx))
                    .link_preview(false)
                    .reply_markup(welcome_markup(ctx)),
            )
            .await
            .map_err(|err| anyhow::anyhow!("failed to send the welcome card: {err}"))?;
        Ok(())
    }

    pub async fn uptime(&self) -> Option<Duration> {
        self.started
            .lock()
            .ok()
            .and_then(|started| *started)
            .map(|started| started.elapsed())
    }

    fn set_status(&self, update: impl FnOnce(&mut InlineStatus)) {
        if let Ok(mut status) = self.status.lock() {
            update(&mut status);
        }
    }

    /// Stop the bot connection and clear the runtime. Safe to call repeatedly.
    pub async fn stop(&self) {
        let runtime = self.runtime.lock().await.take();
        if let Ok(mut started) = self.started.lock() {
            *started = None;
        }
        let Some(runtime) = runtime else {
            self.set_status(|status| status.running = false);
            return;
        };

        info!("Stopping inline bot @{}", runtime.username);
        runtime.updates_task.abort();
        runtime.handle.quit();
        let _ = tokio::time::timeout(Duration::from_secs(6), runtime.pool_task).await;
        self.set_status(|status| {
            status.running = false;
            status.username = None;
            status.bot_id = None;
        });
    }

    /// Start (or restart) the bot from the stored token.
    pub async fn start(self: &Arc<Self>, ctx: &Arc<Ctx>) -> Result<()> {
        self.stop().await;

        let Some(token) = Self::stored_token(ctx) else {
            self.set_status(|status| {
                status.configured = false;
                status.running = false;
                status.error = None;
            });
            return Ok(());
        };

        self.set_status(|status| {
            status.configured = true;
            status.error = None;
        });

        match self.spawn_bot(ctx, &token).await {
            Ok(()) => Ok(()),
            Err(err) => {
                let message = format!("{err:#}");
                warn!("Inline bot failed to start: {message}");
                self.set_status(|status| {
                    status.running = false;
                    status.error = Some(message);
                });
                Err(err)
            }
        }
    }

    /// Restart after a token/username change. Errors are reported to the caller
    /// but do not stop the userbot.
    pub async fn restart(self: &Arc<Self>, ctx: &Arc<Ctx>) -> Result<()> {
        self.restarting.store(true, Ordering::Relaxed);
        let result = self.start(ctx).await;
        self.restarting.store(false, Ordering::Relaxed);
        result
    }

    async fn spawn_bot(self: &Arc<Self>, ctx: &Arc<Ctx>, token: &str) -> Result<()> {
        let bot_uid = token
            .split(':')
            .next()
            .filter(|uid| !uid.is_empty())
            .unwrap_or("bot");
        let sessions = Config::sessions_dir(&ctx.data_root);
        fs::create_dir_all(&sessions).ok();
        cleanup_stale_sessions(&sessions, bot_uid);

        let session_path = sessions.join(format!("inline-{bot_uid}.session"));
        let session = Arc::new(
            SqliteSession::open(&session_path)
                .await
                .with_context(|| format!("failed to open {}", session_path.display()))?,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&session_path, fs::Permissions::from_mode(0o600));
        }

        let SenderPool {
            runner,
            updates,
            handle,
        } = SenderPool::new(session, ctx.config.api_id);
        let client = Client::new(handle.clone());
        let pool_task = tokio::spawn(runner.run());

        let me = match async {
            if !client.is_authorized().await? {
                client
                    .bot_sign_in(token, &ctx.config.api_hash)
                    .await
                    .map_err(|err| anyhow::anyhow!("bot_sign_in failed: {err}"))?;
            }
            client
                .get_me()
                .await
                .map_err(|err| anyhow::anyhow!("get_me failed: {err}"))
        }
        .await
        {
            Ok(me) => me,
            Err(err) => {
                handle.quit();
                let _ = tokio::time::timeout(Duration::from_secs(4), pool_task).await;
                return Err(err);
            }
        };

        let username = me
            .username()
            .map(ToOwned::to_owned)
            .context("the bot has no username; enable inline mode in @BotFather")?;
        let bot_id = me.id().bot_api_dialog_id_unchecked();

        let mut updates = match client
            .stream_updates(updates, UpdatesConfiguration { catch_up: false })
            .await
        {
            Ok(updates) => updates,
            Err(err) => {
                handle.quit();
                let _ = tokio::time::timeout(Duration::from_secs(4), pool_task).await;
                return Err(anyhow::anyhow!("failed to start bot updates: {err}"));
            }
        };

        let manager = self.clone();
        let ctx_clone = ctx.clone();
        let bot_client = client.clone();
        let updates_task = tokio::spawn(async move {
            loop {
                match updates.next().await {
                    Ok(update) => {
                        manager.handle_update(&ctx_clone, &bot_client, update).await;
                    }
                    Err(err) => {
                        warn!("Inline bot update stream ended: {err:#}");
                        break;
                    }
                }
            }
        });

        *self.runtime.lock().await = Some(BotRuntime {
            client,
            bot_id,
            username: username.clone(),
            handle,
            pool_task,
            updates_task,
        });
        if let Ok(mut started) = self.started.lock() {
            *started = Some(Instant::now());
        }
        self.set_status(|status| {
            status.configured = true;
            status.running = true;
            status.username = Some(username.clone());
            status.bot_id = Some(bot_id);
            status.error = None;
        });

        info!("Inline bot @{username} is running (id {bot_id})");
        let _ = ctx
            .db
            .set(INLINE_NS, "bot_username", Value::String(username));
        Ok(())
    }

    async fn handle_update(&self, ctx: &Arc<Ctx>, client: &Client, update: Update) {
        match update {
            Update::InlineQuery(query) => {
                if let Err(err) = self.handle_inline_query(ctx, &query).await {
                    warn!("Inline query failed: {err:#}");
                }
            }
            Update::CallbackQuery(call) => {
                if let Err(err) = self.handle_callback(ctx, &call).await {
                    warn!("Inline callback failed: {err:#}");
                }
            }
            Update::NewMessage(message) => {
                if let Err(err) = self.handle_bot_message(ctx, client, &message).await {
                    debug!("Bot PM handler failed: {err:#}");
                }
            }
            Update::InlineSend(send) => {
                debug!(
                    "Inline result chosen by {} for query {:?}",
                    send.sender_id().bot_api_dialog_id_unchecked(),
                    send.text()
                );
            }
            Update::Raw(raw) => {
                if let tl::enums::Update::MessagePollVote(vote) = raw.raw {
                    if let Err(err) =
                        crate::modules::loader_restrictor::handle_poll_vote(ctx, &vote).await
                    {
                        debug!("Poll vote handler failed: {err:#}");
                    }
                }
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------
    // Inline queries
    // ------------------------------------------------------------------

    async fn handle_inline_query(&self, ctx: &Arc<Ctx>, query: &InlineQuery) -> Result<()> {
        let user_id = query.sender_id().bot_api_dialog_id_unchecked();
        if !self.inline_allowed(ctx, user_id) {
            let article = Article::new(
                ctx.tr("inline.inline_forbidden"),
                InputMessage::new().html(ctx.tr("inline.inline_forbidden_msg")),
            )
            .id(random_id());
            query.answer(vec![article]).cache_time(0).send().await?;
            return Ok(());
        }

        let text = query.text().trim().to_owned();
        let results = self.build_results(ctx, user_id, &text).await;
        let mut answer = query.answer(results).cache_time(0);
        answer = answer.switch_pm(ctx.tr("inline.open_pm"), "start");
        answer.send().await?;
        Ok(())
    }

    fn inline_allowed(&self, ctx: &Ctx, user_id: i64) -> bool {
        if ctx.security.is_owner(user_id) || ctx.security.is_sudo(user_id) {
            return true;
        }
        if ctx.db.get_bool(SECURITY_NS, "allow_inline_query", false) {
            return true;
        }
        ctx.security.all_users().contains(&user_id)
    }

    async fn build_results(&self, ctx: &Arc<Ctx>, user_id: i64, query: &str) -> Vec<Article> {
        let query = query.trim();
        let lower = query.to_lowercase();
        let mut results = Vec::new();

        // Terminal-in-inline, mirroring the Python terminal module.
        let exec_query = query
            .strip_prefix("exec ")
            .or_else(|| query.strip_prefix("terminal "))
            .or_else(|| query.strip_prefix("$ "));
        if let Some(command) = exec_query {
            if ctx.security.is_owner(user_id) || ctx.security.is_sudo(user_id) {
                results.push(self.exec_result(ctx, command.trim()).await);
                return results;
            }
            let denied = Article::new(
                ctx.tr("inline.inline_forbidden"),
                InputMessage::new().html(ctx.tr("inline.inline_forbidden_msg")),
            )
            .id(random_id());
            return vec![denied];
        }

        if query.is_empty() {
            results.push(self.heroku_card(ctx));
            results.push(self.help_card(ctx));
            for command in command_catalog(ctx).into_iter().take(20) {
                results.push(self.command_card(ctx, &command, None));
            }
            return results;
        }

        if lower == "help" || lower == "commands" {
            results.push(self.help_card(ctx));
            results.push(self.heroku_card(ctx));
            for command in command_catalog(ctx).into_iter().take(30) {
                results.push(self.command_card(ctx, &command, None));
            }
            return results;
        }

        if lower == "profile" || lower == "me" {
            results.push(self.profile_card(ctx));
            results.push(self.heroku_card(ctx));
            return results;
        }

        // Always put the Heroku card first for easy access.
        results.push(self.heroku_card(ctx));

        let mut matched = 0usize;

        for module in ctx.modules.all() {
            for command in module.commands() {
                let hit = command.name.to_lowercase().contains(&lower)
                    || command.description.to_lowercase().contains(&lower)
                    || module.name().to_lowercase().contains(&lower)
                    || command
                        .aliases
                        .iter()
                        .any(|alias| alias.to_lowercase().contains(&lower));
                if !hit {
                    continue;
                }
                let invocation = invocation_for(ctx, command.name, query);
                let info = command_info(module.name(), &command);
                results.push(self.command_card(ctx, &info, Some(invocation)));
                matched += 1;
                if matched >= 30 {
                    break;
                }
            }
            if matched >= 30 {
                break;
            }
        }

        if matched == 0 {
            for item in command_catalog(ctx).into_iter().take(20) {
                results.push(self.command_card(ctx, &item, None));
            }
            results.push(
                Article::new(
                    ctx.tr("inline.no_results"),
                    InputMessage::new()
                        .html(ctx.tf("inline.no_results_msg", &[&escape_html(query)])),
                )
                .id(random_id())
                .description(ctx.tr("inline.no_results_desc")),
            );
        }

        results
    }

    async fn exec_result(&self, ctx: &Arc<Ctx>, command: &str) -> Article {
        if command.is_empty() {
            return Article::new(
                ctx.tr("inline.exec_hint"),
                InputMessage::new().html(ctx.tr("inline.exec_hint_msg")),
            )
            .id(random_id())
            .description(ctx.tr("inline.exec_hint_desc"));
        }

        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new("sh")
                .arg("-lc")
                .arg(command)
                .current_dir(&ctx.data_root)
                .output(),
        )
        .await;

        let text = match output {
            Ok(Ok(output)) => {
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                if text.trim().is_empty() {
                    format!("✅ exit code: {}", output.status.code().unwrap_or(-1))
                } else {
                    text
                }
            }
            Ok(Err(err)) => format!("🚫 {err}"),
            Err(_) => "🚫 Command timed out after 20 seconds".to_owned(),
        };

        let preview = text.lines().next().unwrap_or_default();
        let title = format!("💻 {}", truncate(preview, 60));
        let body = format!(
            "<b>💻 Terminal</b>\n<code>{}</code>\n\n<pre><code>{}</code></pre>",
            escape_html(command),
            escape_html(&truncate(&text, 3400))
        );
        Article::new(title, InputMessage::new().html(body))
            .id(random_id())
            .description(truncate(command, 90))
    }

    fn heroku_card(&self, ctx: &Arc<Ctx>) -> Article {
        let uptime = ctx.uptime();
        let modules = ctx.modules.all().len();
        let commands: usize = ctx
            .modules
            .all()
            .iter()
            .map(|module| module.commands().len())
            .sum();
        let text = ctx.tn(
            "inline.heroku_card",
            &[
                ("version", crate::config::VERSION),
                ("uptime", &uptime),
                ("modules", &modules.to_string()),
                ("commands", &commands.to_string()),
                ("prefix", &escape_html(&prefix(ctx))),
            ],
        );
        let markup = ReplyMarkup::from_buttons(&[
            vec![
                Button::url("GitHub", GITHUB_URL),
                Button::url("Support", SUPPORT_URL),
            ],
            vec![
                Button::data("⚙️ Profile", b"heroku:profile".to_vec()),
                Button::switch("📚 Help", ""),
            ],
        ]);
        Article::new(
            format!("🪐 Heroku {}", crate::config::VERSION),
            InputMessage::new().html(text).reply_markup(markup),
        )
        .id(random_id())
        .description(ctx.tr("inline.heroku_card_desc"))
        .thumb_url(DEFAULT_THUMB)
    }

    fn help_card(&self, ctx: &Arc<Ctx>) -> Article {
        let mut lines = String::new();
        let mut seen = std::collections::HashSet::new();
        for module in ctx.modules.all() {
            for command in module.commands() {
                if !seen.insert(command.name) {
                    continue;
                }
                lines.push_str(&format!(
                    "▫️ <code>{}{}</code> — {}\n",
                    escape_html(&prefix(ctx)),
                    escape_html(command.name),
                    escape_html(command.description)
                ));
            }
        }
        let text = ctx.tf("inline.help_card", &[&lines]);
        let markup = ReplyMarkup::from_buttons_row(&[Button::url("GitHub", GITHUB_URL)]);
        Article::new(
            ctx.tr("inline.show_inline_cmds"),
            InputMessage::new().html(text).reply_markup(markup),
        )
        .id(random_id())
        .description(ctx.tr("inline.help_card_desc"))
        .thumb_url(DEFAULT_THUMB)
    }

    fn profile_card(&self, ctx: &Arc<Ctx>) -> Article {
        let markup = ReplyMarkup::from_buttons(&[
            vec![
                Button::data("🔄 Restart", b"heroku:restart".to_vec()),
                Button::data("⌨️ Reset prefix", b"heroku:reset_prefix".to_vec()),
            ],
            vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
        ]);
        Article::new(
            ctx.tr("inline.profile_title"),
            InputMessage::new()
                .html(self.profile_text(ctx, None))
                .reply_markup(markup),
        )
        .id(random_id())
        .description(ctx.tr("inline.profile_desc"))
        .thumb_url(DEFAULT_THUMB)
    }

    fn command_card(
        &self,
        ctx: &Arc<Ctx>,
        command: &CommandInfo,
        invocation: Option<String>,
    ) -> Article {
        let invocation = invocation.unwrap_or_else(|| format!("{}{}", prefix(ctx), command.name));
        let usage = if command.usage.is_empty() {
            String::new()
        } else {
            format!(
                "\n\n<b>{}</b>: <code>{}{} {}</code>",
                escape_html(&ctx.tr("help.usage")),
                escape_html(&prefix(ctx)),
                escape_html(&command.name),
                escape_html(&command.usage)
            )
        };
        let aliases = if command.aliases.is_empty() {
            String::new()
        } else {
            format!(
                "\n<b>{}</b>: <code>{}</code>",
                escape_html(&ctx.tr("help.aliases")),
                command
                    .aliases
                    .iter()
                    .map(|alias| escape_html(alias))
                    .collect::<Vec<_>>()
                    .join("</code>, <code>")
            )
        };
        let body = format!(
            "🪐 <b>{}</b>\n\n{}\n\n<b>{}</b>: <code>{}</code>{}{}",
            escape_html(&command.module),
            escape_html(&command.description),
            escape_html(&ctx.tr("inline.send_command")),
            escape_html(&invocation),
            usage,
            aliases
        );
        let markup = if command.module.eq_ignore_ascii_case("TerminalMod") {
            Some(ReplyMarkup::from_buttons_row(&[Button::switch(
                "▶️ Try in inline",
                format!("exec {}", command.name),
            )]))
        } else {
            None
        };
        let mut message = InputMessage::new().html(body);
        if let Some(markup) = markup {
            message = message.reply_markup(markup);
        }
        Article::new(format!("⚡ {}{}", prefix(ctx), command.name), message)
            .id(random_id())
            .description(truncate(&command.description, 120))
            .thumb_url(DEFAULT_THUMB)
    }

    fn profile_text(&self, ctx: &Arc<Ctx>, username: Option<&str>) -> String {
        let prefix = escape_html(&prefix(ctx));
        let uptime = ctx.uptime();
        let bot = username
            .map(|username| format!("@{username}"))
            .or_else(|| {
                self.status
                    .lock()
                    .ok()
                    .and_then(|status| status.username.clone())
                    .map(|username| format!("@{username}"))
            })
            .unwrap_or_else(|| "—".to_owned());
        ctx.tn(
            "inline.profile_text",
            &[
                ("name", &escape_html(&ctx.me_name)),
                ("id", &ctx.me_id.to_string()),
                ("prefix", &prefix),
                ("uptime", &uptime),
                ("bot", &escape_html(&bot)),
                ("ram", &ram_usage()),
                ("cpu", &cpu_usage()),
                ("host", &host_name()),
                ("modules", &ctx.modules.all().len().to_string()),
            ],
        )
    }

    // ------------------------------------------------------------------
    // Bot PM and callbacks
    // ------------------------------------------------------------------

    async fn handle_bot_message(
        &self,
        ctx: &Arc<Ctx>,
        client: &Client,
        message: &Message,
    ) -> Result<()> {
        let text = message.text().trim();
        let sender = message
            .sender_id()
            .map(|id| id.bot_api_dialog_id_unchecked())
            .unwrap_or_default();
        let Some(peer) = message.peer_ref().await.ok().flatten() else {
            return Ok(());
        };

        if text == "/start" || text.starts_with("/start ") {
            self.clear_pending(sender);
        }

        // Python's `LoaderRestrictor` catches `/start lm_verify` in the bot PM.
        if text == "/start lm_verify" {
            let _ = client.delete_messages(peer, &[message.id()]).await;
            if ctx.security.is_owner(sender) {
                if let Err(err) = crate::modules::loader_restrictor::start_quiz(ctx).await {
                    warn!("Failed to start the loader quiz: {err:#}");
                }
            }
            return Ok(());
        }

        // `/start <param>` may be used by other modules later; ignore the init
        // marker quietly, like the Python bot does.
        if text == "/start heroku init" {
            return Ok(());
        }

        if text == "/start" || text.starts_with("/start ") {
            self.send_start(ctx, client, peer, false).await?;
            return Ok(());
        }

        if text == "/profile" || text == "/me" {
            if !(ctx.security.is_owner(sender) || ctx.security.is_sudo(sender)) {
                client
                    .send_message(peer, InputMessage::new().html(ctx.tr("inline.owner_only")))
                    .await?;
                return Ok(());
            }
            self.send_start(ctx, client, peer, true).await?;
            return Ok(());
        }

        if text == "/help" {
            client
                .send_message(
                    peer,
                    InputMessage::new()
                        .html(self.bot_help_text(ctx))
                        .reply_markup(ReplyMarkup::from_buttons(&[
                            vec![Button::switch("⚡ Open inline mode", "")],
                            vec![
                                Button::url("GitHub", GITHUB_URL),
                                Button::url("Support", SUPPORT_URL),
                            ],
                        ])),
                )
                .await?;
            return Ok(());
        }

        if let Some(pending) = self.take_pending(sender) {
            if text.is_empty() {
                self.set_pending(sender, pending);
                client
                    .send_message(
                        peer,
                        InputMessage::new().html(ctx.tr("inline.expected_text")),
                    )
                    .await?;
                return Ok(());
            }

            if let PendingInput::ConfigValue { module, key } = &pending {
                let html = apply_config_value(ctx, module, key, text).await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(peer, InputMessage::new().html(html))
                    .await?;
                return Ok(());
            }

            if let PendingInput::SecurityGroupUser { group } = &pending {
                let (html, buttons) =
                    crate::modules::heroku_security::add_group_user_pending(ctx, group, text).await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(
                        peer,
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await?;
                return Ok(());
            }

            if let PendingInput::SecurityGroupRule { group } = &pending {
                let (html, buttons) =
                    crate::modules::heroku_security::add_group_rule_pending(ctx, group, text).await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(
                        peer,
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await?;
                return Ok(());
            }

            if matches!(&pending, PendingInput::OwnerAdd) {
                let (html, buttons) =
                    crate::modules::heroku_security::add_owner_pending(ctx, text).await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(
                        peer,
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await?;
                return Ok(());
            }

            if matches!(&pending, PendingInput::GroupCreate) {
                let (html, buttons) =
                    crate::modules::heroku_security::create_group_pending(ctx, text).await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(
                        peer,
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await?;
                return Ok(());
            }

            if let PendingInput::SudoPassword {
                chat_id,
                message_id,
            } = &pending
            {
                let html = crate::modules::terminal::submit_sudo_password(
                    ctx,
                    *chat_id,
                    *message_id,
                    text,
                )
                .await;
                let _ = client.delete_messages(peer, &[message.id()]).await;
                client
                    .send_message(peer, InputMessage::new().html(html))
                    .await?;
                return Ok(());
            }

            let reply = crate::modules::web::bot_account_step(ctx, &pending, text).await;
            // Codes and 2FA passwords must not stay in the chat history.
            if !matches!(&pending, PendingInput::AccountPhone) {
                let _ = client.delete_messages(peer, &[message.id()]).await;
            }
            client
                .send_message(peer, InputMessage::new().html(reply.html))
                .await?;
            if let Some(next) = reply.next {
                self.set_pending(sender, next);
            }
            return Ok(());
        }

        // Quietly confirm a regular PM so users know the bot is alive.
        client
            .send_message(
                peer,
                InputMessage::new()
                    .html(ctx.tr("inline.bot_pm_hint"))
                    .reply_markup(ReplyMarkup::from_buttons_row(&[Button::switch(
                        "⚡ Open inline mode",
                        "",
                    )])),
            )
            .await?;
        Ok(())
    }

    async fn send_start(
        &self,
        ctx: &Arc<Ctx>,
        client: &Client,
        peer: PeerRef,
        profile: bool,
    ) -> Result<()> {
        let username = self.bot_username().await;
        let caption = if profile {
            self.profile_text(ctx, username.as_deref())
        } else {
            // Python's `bot_watcher` uses `this_is_heroku` here; premium custom
            // emoji are unavailable to the Rust client, so use the plain
            // fallback it prints for non-premium accounts.
            ctx.tf("inline.this_is_heroku", &["🪐", "Heroku"])
        };

        let markup = if profile {
            ReplyMarkup::from_buttons(&[
                vec![
                    Button::data("🔄 Restart", b"heroku:restart".to_vec()),
                    Button::data("⌨️ Reset prefix", b"heroku:reset_prefix".to_vec()),
                ],
                vec![
                    Button::url("GitHub", GITHUB_URL),
                    Button::data("✖️ Close", b"heroku:close".to_vec()),
                ],
            ])
        } else {
            ReplyMarkup::from_buttons(&[
                vec![
                    Button::url("GitHub", GITHUB_URL),
                    Button::url("Support", SUPPORT_URL),
                ],
                vec![
                    Button::data("⚙️ Profile", b"heroku:profile".to_vec()),
                    Button::data("📚 Help", b"heroku:help".to_vec()),
                ],
            ])
        };

        let photo = InputMessage::new()
            .html(caption.clone())
            .photo_url(DEFAULT_THUMB)
            .reply_markup(markup);
        if let Err(err) = client.send_message(peer, photo).await {
            debug!("Photo start message failed ({err}); sending text only");
            let markup = if profile {
                ReplyMarkup::from_buttons(&[
                    vec![
                        Button::data("🔄 Restart", b"heroku:restart".to_vec()),
                        Button::data("⌨️ Reset prefix", b"heroku:reset_prefix".to_vec()),
                    ],
                    vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
                ])
            } else {
                ReplyMarkup::from_buttons(&[
                    vec![
                        Button::url("GitHub", GITHUB_URL),
                        Button::url("Support", SUPPORT_URL),
                    ],
                    vec![
                        Button::data("⚙️ Profile", b"heroku:profile".to_vec()),
                        Button::data("📚 Help", b"heroku:help".to_vec()),
                    ],
                ])
            };
            client
                .send_message(peer, InputMessage::new().html(caption).reply_markup(markup))
                .await?;
        }
        Ok(())
    }

    fn bot_help_text(&self, ctx: &Arc<Ctx>) -> String {
        let mut lines = String::new();
        let mut seen = std::collections::HashSet::new();
        for module in ctx.modules.all() {
            for command in module.commands() {
                if !seen.insert(command.name) {
                    continue;
                }
                lines.push_str(&format!(
                    "▫️ <code>{}{}</code> — {}\n",
                    escape_html(&prefix(ctx)),
                    escape_html(command.name),
                    escape_html(command.description)
                ));
            }
        }
        ctx.tf("inline.bot_help", &[&lines])
    }

    // ------------------------------------------------------------------
    // Inline configuration & help galleries
    // ------------------------------------------------------------------

    async fn show_config_page(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        module_index: Option<usize>,
    ) {
        let card = match module_index {
            Some(index) => config_module_card(ctx, index),
            None => Some(config_list_card(ctx)),
        };
        let Some((html, buttons)) = card else {
            let _ = call
                .answer()
                .alert(ctx.tr("config.inline_missing"))
                .send()
                .await;
            return;
        };
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
    }

    async fn handle_config_callback(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        data: &str,
        user_id: i64,
    ) -> Result<()> {
        let parts: Vec<&str> = data.split(':').collect();
        let action = parts.get(2).copied().unwrap_or_default();

        match action {
            "" => self.show_config_page(ctx, call, None).await,
            "m" => {
                let index = parts.get(3).and_then(|value| value.parse().ok());
                self.show_config_page(ctx, call, index).await;
            }
            "t" | "c" | "r" => {
                let index = parts.get(3).and_then(|value| value.parse::<usize>().ok());
                let Some(index) = index else {
                    let _ = call.answer().send().await;
                    return Ok(());
                };
                let Some(module) = ctx.modules.all().get(index) else {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("config.inline_missing"))
                        .send()
                        .await;
                    return Ok(());
                };
                let specs = module.config_spec();

                if action == "r" {
                    for spec in &specs {
                        ctx.db
                            .set_value(module.name(), spec.key, spec.default.to_value());
                    }
                    let _ = call.answer().text(ctx.tr("config.reset_done")).send().await;
                } else {
                    let spec_index = parts.get(4).and_then(|value| value.parse::<usize>().ok());
                    let Some(spec) = spec_index.and_then(|spec_index| specs.get(spec_index)) else {
                        let _ = call
                            .answer()
                            .alert(ctx.tr("config.inline_missing"))
                            .send()
                            .await;
                        return Ok(());
                    };
                    let current = ctx
                        .db
                        .get_value(module.name(), spec.key)
                        .unwrap_or_else(|| spec.default.to_value());
                    match (action, spec.kind) {
                        ("t", ConfigKind::Bool) => {
                            let value = current.as_bool().unwrap_or(false);
                            ctx.db.set(module.name(), spec.key, !value);
                            let _ = call.answer().send().await;
                        }
                        ("c", ConfigKind::Choice(choices)) => {
                            let current =
                                current.as_str().map(ToOwned::to_owned).unwrap_or_default();
                            let position = choices
                                .iter()
                                .position(|choice| *choice == current)
                                .unwrap_or(0);
                            let next = choices[(position + 1) % choices.len()];
                            ctx.db.set(module.name(), spec.key, next);
                            let _ = call.answer().text(next.to_owned()).send().await;
                        }
                        _ => {
                            self.set_pending(
                                user_id,
                                PendingInput::ConfigValue {
                                    module: module.name(),
                                    key: spec.key,
                                },
                            );
                            let prompt = ctx.tf(
                                "config.inline_prompt",
                                &[
                                    &escape_html(module.name()),
                                    &escape_html(spec.key),
                                    &escape_html(&crate::modules::common::show_value(&current)),
                                ],
                            );
                            let _ = call
                                .answer()
                                .text(ctx.tr("config.inline_waiting"))
                                .edit(InputMessage::new().html(prompt))
                                .await;
                            return Ok(());
                        }
                    }
                }

                self.show_config_page(ctx, call, Some(index)).await;
            }
            _ => {
                let _ = call.answer().send().await;
            }
        }
        Ok(())
    }

    async fn handle_security_callback(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        data: &str,
        user_id: i64,
    ) -> Result<()> {
        let parts: Vec<&str> = data.split(':').collect();
        let action = parts.get(2).copied().unwrap_or_default();

        let (html, buttons) = match action {
            "o" => match parts.get(3).copied().unwrap_or_default() {
                "" => crate::modules::heroku_security::owner_card(ctx),
                "cf" => {
                    let user = parts
                        .get(4)
                        .and_then(|value| value.parse::<i64>().ok())
                        .unwrap_or_default();
                    let nonick = parts.get(5).copied() == Some("1");
                    let prefix = parts
                        .get(6)
                        .and_then(|value| crate::modules::heroku_security::hex_decode(value))
                        .map(|value| {
                            value
                                .split('\n')
                                .filter(|item| !item.is_empty())
                                .map(ToOwned::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    crate::modules::heroku_security::apply_owner_add(ctx, user, nonick, prefix)
                        .await
                }
                "add" => {
                    self.set_pending(user_id, PendingInput::OwnerAdd);
                    let _ = call
                        .answer()
                        .text(ctx.tr("security.inline_waiting"))
                        .edit(InputMessage::new().html(ctx.tr("security.inline_owner_prompt")))
                        .await;
                    return Ok(());
                }
                "rm" => {
                    if let Some(id) = parts.get(4).and_then(|value| value.parse::<i64>().ok()) {
                        if id != ctx.me_id {
                            ctx.security.remove_owner(id, ctx.me_id);
                        }
                    }
                    crate::modules::heroku_security::owner_card(ctx)
                }
                _ => crate::modules::heroku_security::owner_card(ctx),
            },
            "g" => {
                let name = parts.get(3).copied().unwrap_or_default();
                match name {
                    "" => crate::modules::heroku_security::groups_card(ctx),
                    "new" => {
                        self.set_pending(user_id, PendingInput::GroupCreate);
                        let _ = call
                            .answer()
                            .text(ctx.tr("security.inline_waiting"))
                            .edit(InputMessage::new().html(ctx.tr("security.inline_group_prompt")))
                            .await;
                        return Ok(());
                    }
                    name => match parts.get(4).copied().unwrap_or_default() {
                        "" => crate::modules::heroku_security::group_card(ctx, name),
                        "u" => {
                            self.set_pending(
                                user_id,
                                PendingInput::SecurityGroupUser {
                                    group: name.to_owned(),
                                },
                            );
                            let prompt =
                                ctx.tf("security.inline_user_prompt", &[&escape_html(name)]);
                            let _ = call
                                .answer()
                                .text(ctx.tr("security.inline_waiting"))
                                .edit(InputMessage::new().html(prompt))
                                .await;
                            return Ok(());
                        }
                        "r" => {
                            self.set_pending(
                                user_id,
                                PendingInput::SecurityGroupRule {
                                    group: name.to_owned(),
                                },
                            );
                            let prompt =
                                ctx.tf("security.inline_rule_prompt", &[&escape_html(name)]);
                            let _ = call
                                .answer()
                                .text(ctx.tr("security.inline_waiting"))
                                .edit(InputMessage::new().html(prompt))
                                .await;
                            return Ok(());
                        }
                        "d" => {
                            ctx.security.delete_sgroup(name);
                            let _ = call
                                .answer()
                                .text(ctx.tr("security.sgroup_deleted"))
                                .send()
                                .await;
                            crate::modules::heroku_security::groups_card(ctx)
                        }
                        "rm" => {
                            if let Some(id) =
                                parts.get(5).and_then(|value| value.parse::<i64>().ok())
                            {
                                ctx.security.remove_sgroup_user(name, id);
                            }
                            crate::modules::heroku_security::group_card(ctx, name)
                        }
                        _ => crate::modules::heroku_security::group_card(ctx, name),
                    },
                }
            }
            "p" => {
                let page = parts
                    .get(3)
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                crate::modules::heroku_security::permissions_card(ctx, page)
            }
            "pm" => {
                let module = parts.get(3).copied().unwrap_or_default();
                let command = parts.get(4).copied().unwrap_or_default();
                crate::modules::heroku_security::command_mask_card(ctx, module, command)
                    .unwrap_or_else(|| crate::modules::heroku_security::permissions_card(ctx, 0))
            }
            "pb" => {
                let module = parts.get(3).copied().unwrap_or_default();
                let command = parts.get(4).copied().unwrap_or_default();
                let bit = parts
                    .get(5)
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0);
                crate::modules::heroku_security::toggle_command_bit(ctx, module, command, bit)
                    .unwrap_or_else(|| crate::modules::heroku_security::permissions_card(ctx, 0))
            }
            "pr" => {
                let module = parts.get(3).copied().unwrap_or_default();
                let command = parts.get(4).copied().unwrap_or_default();
                if let Some(module_ref) = ctx.modules.get(module) {
                    if let Some(command_ref) = module_ref
                        .commands()
                        .into_iter()
                        .find(|item| item.name.eq_ignore_ascii_case(command))
                    {
                        ctx.security
                            .clear_command_mask(module_ref.name(), command_ref.name);
                    }
                }
                crate::modules::heroku_security::command_mask_card(ctx, module, command)
                    .unwrap_or_else(|| crate::modules::heroku_security::permissions_card(ctx, 0))
            }
            "pdef" => crate::modules::heroku_security::global_mask_card(ctx, false),
            "pbnd" => match parts.get(3).and_then(|value| value.parse::<u32>().ok()) {
                Some(bit) => crate::modules::heroku_security::toggle_global_bit(ctx, true, bit),
                None => crate::modules::heroku_security::global_mask_card(ctx, true),
            },
            "pd" => match parts.get(3).and_then(|value| value.parse::<u32>().ok()) {
                Some(bit) => crate::modules::heroku_security::toggle_global_bit(ctx, false, bit),
                None => crate::modules::heroku_security::global_mask_card(ctx, false),
            },
            "query" => crate::modules::heroku_security::toggle_querysec(ctx),
            "nn" => {
                let user = parts
                    .get(3)
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or_default();
                let html = crate::modules::heroku_security::enable_nonick(ctx, user).await;
                (html, Vec::new())
            }
            "tc" => {
                let id = parts
                    .get(3)
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0);
                match crate::modules::heroku_security::choose_rule(ctx, id).await {
                    Some(card) => card,
                    None => crate::modules::heroku_security::security_card(ctx),
                }
            }
            "tr" => {
                let id = parts
                    .get(3)
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0);
                (
                    crate::modules::heroku_security::apply_rule(ctx, id),
                    Vec::new(),
                )
            }
            _ => crate::modules::heroku_security::security_card(ctx),
        };

        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
        Ok(())
    }

    async fn show_help_page(&self, ctx: &Arc<Ctx>, call: &CallbackQuery, page: usize) {
        let (html, buttons) = help_page_card(ctx, page);
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
    }

    async fn show_help_module(&self, ctx: &Arc<Ctx>, call: &CallbackQuery, index: usize) {
        let Some((html, buttons)) = help_module_card(ctx, index) else {
            let _ = call.answer().alert(ctx.tr("help.no_module")).send().await;
            return;
        };
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
    }

    async fn handle_callback(&self, ctx: &Arc<Ctx>, call: &CallbackQuery) -> Result<()> {
        let data = String::from_utf8_lossy(call.data()).to_string();
        let sender = call.sender_id().bot_api_dialog_id_unchecked();
        let allowed = ctx.security.is_owner(sender) || ctx.security.is_sudo(sender);

        match data.as_str() {
            "heroku:profile" => {
                let _ = call
                    .answer()
                    .edit(
                        InputMessage::new()
                            .html(self.profile_text(ctx, None))
                            .reply_markup(ReplyMarkup::from_buttons(&[
                                vec![
                                    Button::data("🔄 Restart", b"heroku:restart".to_vec()),
                                    Button::data(
                                        "⌨️ Reset prefix",
                                        b"heroku:reset_prefix".to_vec(),
                                    ),
                                ],
                                vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
                            ])),
                    )
                    .await;
            }
            "heroku:help" => {
                self.show_help_page(ctx, call, 0).await;
            }
            _ if data.starts_with("heroku:help:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let parts: Vec<&str> = data.split(':').collect();
                match parts.get(2).copied() {
                    Some("p") => {
                        let page = parts
                            .get(3)
                            .and_then(|value| value.parse::<usize>().ok())
                            .unwrap_or(0);
                        self.show_help_page(ctx, call, page).await;
                    }
                    Some("m") => {
                        if let Some(index) = parts.get(3).and_then(|value| value.parse().ok()) {
                            self.show_help_module(ctx, call, index).await;
                        } else {
                            let _ = call.answer().send().await;
                        }
                    }
                    _ => {
                        let _ = call.answer().send().await;
                    }
                }
            }
            _ if data == "heroku:cfg" || data.starts_with("heroku:cfg:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                self.handle_config_callback(ctx, call, &data, sender)
                    .await?;
            }
            _ if data.starts_with("heroku:backup:period:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let hours = data
                    .trim_start_matches("heroku:backup:period:")
                    .parse::<i64>()
                    .unwrap_or(0);
                let (html, buttons) =
                    crate::modules::heroku_backup::set_period_from_inline(ctx, hours);
                let text = if hours == 0 {
                    ctx.tr("backup.period_never")
                } else {
                    ctx.tf("backup.period_set", &[&hours.to_string()])
                };
                let _ = call
                    .answer()
                    .text(text)
                    .edit(
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await;
            }
            "heroku:api:toggle" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let (html, buttons) = crate::modules::api_ratelimiter::toggle_protection(ctx);
                let _ = call
                    .answer()
                    .edit(
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await;
            }
            _ if data.starts_with("heroku:set:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let key = data.trim_start_matches("heroku:set:");
                if let Some((html, buttons)) =
                    crate::modules::heroku_settings::toggle_setting(ctx, key)
                {
                    let _ = call
                        .answer()
                        .edit(
                            InputMessage::new()
                                .html(html)
                                .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                        )
                        .await;
                } else {
                    let _ = call.answer().send().await;
                }
            }
            _ if data == "heroku:sec" || data.starts_with("heroku:sec:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                self.handle_security_callback(ctx, call, &data, sender)
                    .await?;
            }
            "heroku:restart" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let _ = call
                    .answer()
                    .text(ctx.tr("inline.restart_requested"))
                    .edit(InputMessage::new().html(ctx.tr("inline.restart_message")))
                    .await;
                let db = ctx.db.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    db.save();
                    if let Err(err) = restart_process() {
                        error!("Process restart failed: {err}");
                    }
                });
            }
            "heroku:reset_prefix" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                ctx.db.set("heroku.main", "command_prefix", ".");
                ctx.db.set(
                    "heroku.main",
                    "command_prefix_aliases",
                    Value::Array(Vec::new()),
                );
                let _ = call
                    .answer()
                    .text(ctx.tr("inline.prefix_reset"))
                    .edit(
                        InputMessage::new()
                            .html(self.profile_text(ctx, None))
                            .reply_markup(ReplyMarkup::from_buttons(&[
                                vec![
                                    Button::data("🔄 Restart", b"heroku:restart".to_vec()),
                                    Button::data(
                                        "⌨️ Reset prefix",
                                        b"heroku:reset_prefix".to_vec(),
                                    ),
                                ],
                                vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
                            ])),
                    )
                    .await;
            }
            "heroku:upd:apply" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let _ = call
                    .answer()
                    .text(ctx.tr("updater.updating"))
                    .edit(InputMessage::new().html(ctx.tr("updater.updating")))
                    .await;
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    crate::modules::updater::inline_apply_update(ctx).await;
                });
            }
            "heroku:upd:restart" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let _ = call
                    .answer()
                    .text(ctx.tr("inline.restart_requested"))
                    .edit(InputMessage::new().html(ctx.tr("inline.restart_message")))
                    .await;
                ctx.db.save();
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(600)).await;
                    if let Err(err) = restart_process() {
                        error!("Process restart failed: {err}");
                        std::process::exit(0);
                    }
                });
            }
            "heroku:upd:stop" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let _ = call
                    .answer()
                    .edit(InputMessage::new().html(ctx.tr("updater.stopped")))
                    .await;
                ctx.db.save();
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    std::process::exit(0);
                });
            }
            _ if data.starts_with("heroku:upd:rb:") || data.starts_with("heroku:upd:rbb:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let build = data.starts_with("heroku:upd:rbb:");
                let target = data
                    .trim_start_matches("heroku:upd:rbb:")
                    .trim_start_matches("heroku:upd:rb:")
                    .to_owned();
                if target.is_empty() {
                    let _ = call.answer().send().await;
                    return Ok(());
                }
                let _ = call
                    .answer()
                    .text(ctx.tr("updater.building"))
                    .edit(InputMessage::new().html(ctx.tr("updater.building")))
                    .await;
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        crate::modules::updater::rollback_from_inline(&ctx, &target, build).await
                    {
                        let _ = ctx
                            .send_self(ctx.tf(
                                "updater.update_failed",
                                &[&escape_html(&format!("{err:#}"))],
                            ))
                            .await;
                    }
                });
            }
            "heroku:close" => {
                let _ = call
                    .answer()
                    .edit(InputMessage::new().html(ctx.tr("inline.menu_closed")))
                    .await;
            }
            _ if data == "heroku:acc"
                || data == "heroku:acc:list"
                || data == "heroku:acc:add"
                || data == "heroku:acc:default"
                || data == "heroku:acc:resend"
                || data.starts_with("heroku:acc:addtarget:")
                || data.starts_with("heroku:acc:sw:")
                || data.starts_with("heroku:acc:swc:") =>
            {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }

                if data == "heroku:acc:add" || data.starts_with("heroku:acc:addtarget:") {
                    self.set_pending(sender, PendingInput::AccountPhone);
                    let _ = call
                        .answer()
                        .text(ctx.tr("accounts.inline_phone_hint"))
                        .edit(InputMessage::new().html(ctx.tr("accounts.inline_phone_prompt")))
                        .await;
                    return Ok(());
                }

                if data == "heroku:acc:resend" {
                    let html = crate::modules::web::resend_inline(ctx).await;
                    if ctx.db.get_str("HerokuAccounts", "pending_step", "") == "code" {
                        self.set_pending(sender, PendingInput::AccountCode);
                    }
                    let _ = call
                        .answer()
                        .text(ctx.tr("accounts.inline_resend_done"))
                        .edit(InputMessage::new().html(html))
                        .await;
                    return Ok(());
                }

                if data == "heroku:acc:default" {
                    let (html, buttons) = crate::modules::web::switch_prompt(ctx, "default");
                    let rows = buttons.unwrap_or_else(|| {
                        vec![vec![Button::data("⬅️ Back", b"heroku:acc:list".to_vec())]]
                    });
                    let _ = call
                        .answer()
                        .edit(
                            InputMessage::new()
                                .html(html)
                                .reply_markup(ReplyMarkup::from_buttons(&rows)),
                        )
                        .await;
                    return Ok(());
                }

                if let Some(id) = data.strip_prefix("heroku:acc:sw:") {
                    let (html, buttons) = crate::modules::web::switch_prompt(ctx, id);
                    let rows = buttons.unwrap_or_else(|| {
                        vec![vec![Button::data("⬅️ Back", b"heroku:acc:list".to_vec())]]
                    });
                    let _ = call
                        .answer()
                        .edit(
                            InputMessage::new()
                                .html(html)
                                .reply_markup(ReplyMarkup::from_buttons(&rows)),
                        )
                        .await;
                    return Ok(());
                }

                if let Some(id) = data.strip_prefix("heroku:acc:swc:") {
                    let html = crate::modules::web::switch_inline(ctx, id).await;
                    let buttons = vec![vec![Button::data("⬅️ Back", b"heroku:acc:list".to_vec())]];
                    let _ = call
                        .answer()
                        .edit(
                            InputMessage::new()
                                .html(html)
                                .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                        )
                        .await;
                    return Ok(());
                }

                let (html, buttons) = crate::modules::web::accounts_card(ctx);
                let _ = call
                    .answer()
                    .edit(
                        InputMessage::new()
                            .html(html)
                            .reply_markup(ReplyMarkup::from_buttons(&buttons)),
                    )
                    .await;
            }
            "heroku:lmverify" => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                let _ = call.answer().send().await;
                if let Ok(message) = call.load_message().await {
                    let _ = message.delete().await;
                }
                if let Err(err) = crate::modules::loader_restrictor::start_quiz(ctx).await {
                    warn!("Failed to start the loader quiz: {err:#}");
                }
            }
            _ if data.starts_with("heroku:mod:") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                if !crate::modules::loader::handle_inline_callback(ctx, call, &data).await? {
                    let _ = call.answer().send().await;
                }
            }
            _ if data.starts_with("heroku:backupall:restore") => {
                if !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                if let Err(err) =
                    crate::modules::heroku_backup::handle_restore_callback(ctx, call, &data).await
                {
                    tracing::warn!("Backup restore callback failed: {err:#}");
                    let _ = call.answer().send().await;
                }
            }
            _ if data.starts_with("heroku:join:") => {
                let mut parts = data.splitn(4, ':');
                let _ = parts.next();
                let _ = parts.next();
                let action = parts.next().unwrap_or_default();
                let username = parts.next().unwrap_or_default();
                if username.is_empty() || !allowed {
                    let _ = call
                        .answer()
                        .alert(ctx.tr("inline.callback_forbidden"))
                        .send()
                        .await;
                    return Ok(());
                }
                if let Err(err) =
                    crate::content::handle_join_callback(ctx, call, action, username).await
                {
                    tracing::warn!("Join callback failed: {err:#}");
                    let _ = call.answer().send().await;
                }
            }
            _ if data.starts_with("heroku:langwelcome/") => {
                let code = crate::translations::canonical_lang(
                    data.trim_start_matches("heroku:langwelcome/"),
                );
                if crate::translations::is_known_lang(&code) {
                    ctx.translator.set_lang(&code);
                    ctx.db.set("heroku.translations", "lang", code);
                    let message = InputMessage::new()
                        .html(welcome_text(ctx))
                        .link_preview(false)
                        .reply_markup(welcome_markup(ctx));
                    let _ = call
                        .answer()
                        .text(ctx.tr("quickstart.language_saved"))
                        .edit(message)
                        .await;
                } else {
                    let _ = call.answer().send().await;
                }
            }
            _ if data.starts_with("heroku:logs:") => {
                if !crate::modules::test::handle_inline_callback(ctx, call, &data).await? {
                    let _ = call.answer().send().await;
                }
            }
            _ if data.starts_with("heroku:lang/") || data.starts_with("heroku:langs/") => {
                if !crate::modules::translations::handle_inline_callback(ctx, call, &data).await? {
                    let _ = call.answer().send().await;
                }
            }
            _ => {
                let _ = call.answer().send().await;
            }
        }
        Ok(())
    }
}

async fn apply_config_value(ctx: &Arc<Ctx>, module_name: &str, key: &str, raw: &str) -> String {
    let Some(module) = ctx.modules.get(module_name) else {
        return ctx.tr("config.inline_missing");
    };
    let Some(spec) = module
        .config_spec()
        .into_iter()
        .find(|spec| spec.key == key)
    else {
        return ctx.tr("config.inline_missing");
    };

    let trimmed = raw.trim();
    let parsed: Result<Value, String> = match spec.kind {
        ConfigKind::Bool => crate::modules::common::parse_bool(trimmed)
            .map(Value::Bool)
            .ok_or_else(|| "expected on/off, true/false or yes/no".to_owned()),
        ConfigKind::Int => trimmed
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| "expected an integer".to_owned()),
        ConfigKind::Float => trimmed
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| "expected a number".to_owned()),
        ConfigKind::Choice(choices) => {
            if choices.contains(&trimmed) {
                Ok(Value::String(trimmed.to_owned()))
            } else {
                Err(format!("expected one of: {}", choices.join(", ")))
            }
        }
        ConfigKind::List => Ok(Value::Array(
            raw.split([',', '\n'])
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::String(item.to_owned()))
                .collect(),
        )),
        ConfigKind::Text | ConfigKind::Hidden => Ok(Value::String(raw.to_owned())),
    };

    let value = match parsed {
        Ok(value) => value,
        Err(reason) => {
            return ctx.tf(
                "config.inline_bad_value",
                &[
                    &escape_html(module.name()),
                    &escape_html(key),
                    &escape_html(&reason),
                ],
            )
        }
    };

    ctx.db.set_value(module.name(), spec.key, value.clone());
    ctx.tf(
        "config.inline_saved",
        &[
            &escape_html(module.name()),
            &escape_html(key),
            &escape_html(&crate::modules::common::show_value(&value)),
        ],
    )
}

fn short_module_name(name: &str) -> &str {
    name.strip_suffix("Mod").unwrap_or(name)
}

fn config_list_card(ctx: &Ctx) -> (String, Vec<Vec<Button>>) {
    let mut lines = vec!["⚙️ <b>Modules with configuration</b>".to_owned()];
    let mut rows = Vec::new();

    for (index, module) in ctx.modules.all().iter().enumerate() {
        let specs = module.config_spec();
        if specs.is_empty() {
            continue;
        }
        let name = short_module_name(module.name());
        lines.push(format!(
            "▫️ <b>{}</b> <code>[{}]</code>",
            escape_html(name),
            specs.len()
        ));
        rows.push(vec![Button::data(
            format!("⚙️ {name}"),
            format!("heroku:cfg:m:{index}").into_bytes(),
        )]);
    }

    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

fn config_module_card(ctx: &Ctx, index: usize) -> Option<(String, Vec<Vec<Button>>)> {
    let module = ctx.modules.all().get(index)?;
    let specs = module.config_spec();
    if specs.is_empty() {
        return None;
    }

    let mut lines = vec![ctx.tf("config.title", &[&escape_html(module.name())])];
    let mut rows = Vec::new();

    for (spec_index, spec) in specs.iter().enumerate() {
        let value = ctx
            .db
            .get_value(module.name(), spec.key)
            .unwrap_or_else(|| spec.default.to_value());
        lines.push(format!(
            "▫️ <code>{}</code> = {}\n   <i>{}</i>",
            escape_html(spec.key),
            crate::modules::common::show_value(&value),
            escape_html(spec.description)
        ));

        let button = match spec.kind {
            ConfigKind::Bool => {
                let enabled = value.as_bool().unwrap_or(false);
                Button::data(
                    format!("{} {}", if enabled { "✅" } else { "❌" }, spec.key),
                    format!("heroku:cfg:t:{index}:{spec_index}").into_bytes(),
                )
            }
            ConfigKind::Choice(_) => Button::data(
                format!(
                    "🔁 {}: {}",
                    spec.key,
                    truncate(value.as_str().unwrap_or("—"), 24)
                ),
                format!("heroku:cfg:c:{index}:{spec_index}").into_bytes(),
            ),
            _ => Button::data(
                format!("⌨️ {}", spec.key),
                format!("heroku:cfg:i:{index}:{spec_index}").into_bytes(),
            ),
        };
        rows.push(vec![button]);
    }

    rows.push(vec![
        Button::data("♻️ Reset", format!("heroku:cfg:r:{index}").into_bytes()),
        Button::data("⬅️ Back", b"heroku:cfg".to_vec()),
    ]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    Some((lines.join("\n"), rows))
}

fn help_page_card(ctx: &Ctx, page: usize) -> (String, Vec<Vec<Button>>) {
    const PAGE_SIZE: usize = 8;
    let hidden = ctx.db.get_string_list("Help", "hide");
    let visible: Vec<(usize, &Arc<dyn crate::module::Module>)> = ctx
        .modules
        .all()
        .iter()
        .enumerate()
        .filter(|(_, module)| {
            !hidden
                .iter()
                .any(|name| name.eq_ignore_ascii_case(module.name()))
        })
        .collect();

    let pages = visible.len().div_ceil(PAGE_SIZE).max(1);
    let page = page.min(pages - 1);
    let start = page * PAGE_SIZE;
    let slice = &visible[start..visible.len().min(start + PAGE_SIZE)];

    let mut lines = vec![
        ctx.tr("help.title"),
        ctx.tf("help.subtitle", &[crate::config::VERSION]),
        format!("<i>Page {}/{pages}</i>", page + 1),
    ];
    let mut rows = Vec::new();

    for chunk in slice.chunks(2) {
        let mut row = Vec::new();
        for (index, module) in chunk {
            let name = short_module_name(module.name());
            lines.push(format!(
                "▫️ <b>{}</b> <code>[{}]</code>",
                escape_html(name),
                module.commands().len()
            ));
            row.push(Button::data(
                format!("📚 {name}"),
                format!("heroku:help:m:{index}").into_bytes(),
            ));
        }
        rows.push(row);
    }

    let mut navigation = Vec::new();
    if page > 0 {
        navigation.push(Button::data(
            "⬅️",
            format!("heroku:help:p:{}", page - 1).into_bytes(),
        ));
    }
    navigation.push(Button::data("🏠", b"heroku:help".to_vec()));
    if page + 1 < pages {
        navigation.push(Button::data(
            "➡️",
            format!("heroku:help:p:{}", page + 1).into_bytes(),
        ));
    }
    rows.push(navigation);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);

    (lines.join("\n"), rows)
}

fn help_module_card(ctx: &Ctx, index: usize) -> Option<(String, Vec<Vec<Button>>)> {
    let module = ctx.modules.all().get(index)?;
    let prefix = prefix(ctx);
    let mut lines = vec![
        ctx.tf("help.module", &[&escape_html(module.name())]),
        format!("<i>{}</i>", escape_html(module.description())),
        String::new(),
    ];

    let commands = module.commands();
    if commands.is_empty() {
        lines.push(ctx.tr("help.no_commands"));
    }
    for command in &commands {
        lines.push(ctx.tf(
            "help.command",
            &[&format!("{prefix}{}", escape_html(command.name))],
        ));
        if !command.usage.is_empty() {
            lines.push(format!("   <code>{}</code>", escape_html(command.usage)));
        }
        if !command.description.is_empty() {
            lines.push(format!("   <i>{}</i>", escape_html(command.description)));
        }
        if !command.aliases.is_empty() {
            let aliases = command
                .aliases
                .iter()
                .map(|alias| format!("{prefix}{alias}"))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "   ↳ <i>{}: {}</i>",
                ctx.tr("help.aliases"),
                escape_html(&aliases)
            ));
        }
        lines.push(String::new());
    }

    let buttons = vec![
        vec![
            Button::data("⬅️ Back", b"heroku:help".to_vec()),
            Button::data("✖️ Close", b"heroku:close".to_vec()),
        ],
        vec![Button::url("GitHub", GITHUB_URL)],
    ];
    Some((lines.join("\n"), buttons))
}

#[derive(Clone)]
struct CommandInfo {
    name: String,
    description: String,
    usage: String,
    aliases: Vec<String>,
    module: String,
}

fn command_info(module: &str, command: &CommandDef) -> CommandInfo {
    CommandInfo {
        name: command.name.to_owned(),
        description: command.description.to_owned(),
        usage: command.usage.to_owned(),
        aliases: command
            .aliases
            .iter()
            .map(|alias| (*alias).to_owned())
            .collect(),
        module: module.to_owned(),
    }
}

fn command_catalog(ctx: &Ctx) -> Vec<CommandInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for module in ctx.modules.all() {
        for command in module.commands() {
            if !seen.insert(command.name.to_owned()) {
                continue;
            }
            out.push(command_info(module.name(), &command));
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn invocation_for(ctx: &Ctx, command: &str, query: &str) -> String {
    let prefix = prefix(ctx);
    let mut words = query.split_whitespace();
    let Some(first) = words.next() else {
        return format!("{prefix}{command}");
    };
    if first.eq_ignore_ascii_case(command) {
        let rest = query[first.len()..].trim();
        if rest.is_empty() {
            format!("{prefix}{command}")
        } else {
            format!("{prefix}{command} {rest}")
        }
    } else {
        format!("{prefix}{command}")
    }
}

fn prefix(ctx: &Ctx) -> String {
    let prefix = ctx.db.get_str("heroku.main", "command_prefix", ".");
    if prefix.is_empty() {
        ".".to_owned()
    } else {
        prefix
    }
}

fn welcome_text(ctx: &Ctx) -> String {
    ctx.tf("quickstart.base", &["Heroku"])
}

fn welcome_markup(ctx: &Ctx) -> ReplyMarkup {
    let mut rows = vec![vec![Button::url(
        ctx.tr("quickstart.btn_support"),
        SUPPORT_URL,
    )]];
    let mut row = Vec::new();
    for code in crate::translations::OFFICIAL_LANGUAGES {
        row.push(Button::data(
            ctx.translator.t_in(code, "quickstart.language"),
            format!("heroku:langwelcome/{code}").into_bytes(),
        ));
        if row.len() == 3 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    ReplyMarkup::from_buttons(&rows)
}

fn random_id() -> String {
    format!("{:016x}", rand::thread_rng().gen::<u64>())
}

fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push('…');
    out
}

fn cleanup_stale_sessions(sessions: &Path, keep_uid: &str) {
    let Ok(entries) = fs::read_dir(sessions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(uid) = name
            .strip_prefix("inline-")
            .and_then(|rest| rest.strip_suffix(".session"))
        else {
            continue;
        };
        if uid == keep_uid {
            continue;
        }
        let _ = fs::remove_file(entry.path());
        debug!("Removed stale inline session {}", entry.path().display());
    }
}

fn ram_usage() -> String {
    let raw = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mut total = 0u64;
    let mut available = 0u64;
    for line in raw.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total = parse_kib(value);
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available = parse_kib(value);
        }
    }
    if total == 0 {
        return "—".to_owned();
    }
    let used = total.saturating_sub(available);
    let percent = (used as f64 / total as f64 * 100.0).round() as u64;
    format!("{} / {} MB ({}%)", used / 1024, total / 1024, percent)
}

fn parse_kib(value: &str) -> u64 {
    value
        .split_whitespace()
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn cpu_usage() -> String {
    let load = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|raw| {
            raw.split_whitespace()
                .next()
                .and_then(|value| value.parse::<f64>().ok())
        })
        .map(|load| format!("load {load:.2}"))
        .unwrap_or_else(|| "—".to_owned());
    format!(
        "{load} · {} cores",
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    )
}

fn host_name() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(unix)]
pub(crate) fn restart_process() -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe()?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let err = std::process::Command::new(exe).args(args).exec();
    Err(err)
}

#[cfg(not(unix))]
pub(crate) fn restart_process() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "restart is supported on Unix only",
    ))
}
