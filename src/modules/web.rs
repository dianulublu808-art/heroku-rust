// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Multi-account management: `.addacc` and `.switchacc`.
//!
//! Every additional account gets its own Telegram session and database under
//! `accounts/<id>/`. `.addacc` performs a real MTProto login in the current
//! chat (code and 2FA password are deleted from the chat afterwards), and
//! `.switchacc` rewrites the account registry and restarts the userbot with the
//! selected account.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use grammers_client::message::Button;
use grammers_client::Client;
use grammers_mtsender::{SenderPool, SenderPoolFatHandle};
use grammers_session::storages::{erase, ErasedSession, SqliteSession};
use tokio::task::JoinHandle;
use tracing::error;

use crate::accounts::{self, Account, Registry};
use crate::args::Args;
use crate::config::Config;
use crate::ctx::Ctx;
use crate::inline::PendingInput;
use crate::login;
use crate::module::{CommandDef, Module, ModuleFactory, OWNER};
use crate::tg::{escape_html, Msg};

const NS: &str = "HerokuAccounts";

pub struct HerokuAccounts;

#[async_trait]
impl Module for HerokuAccounts {
    fn name(&self) -> &'static str {
        "HerokuAccounts"
    }

    fn description(&self) -> &'static str {
        "Add and switch userbot accounts"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("addacc", "Log in an additional Telegram account")
                .usage("<phone> | code <code> | 2fa <password> | resend | cancel")
                .security(OWNER),
            CommandDef::new("switchacc", "Switch the active userbot account")
                .usage("[account|default]")
                .security(OWNER),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        match command {
            "addacc" => self.add(ctx, message, args).await,
            "switchacc" => self.switch(ctx, message, args).await,
            _ => Ok(()),
        }
    }
}

impl HerokuAccounts {
    async fn add(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(action) = args.get(0) else {
            let (html, buttons) = accounts_card(ctx);
            if ctx
                .inline
                .send_to_user(ctx, ctx.me_id, html, buttons)
                .await
                .is_ok()
            {
                return Ok(());
            }
            message.reply_html(ctx.tr("accounts.add_usage")).await?;
            return Ok(());
        };

        match action.to_lowercase().as_str() {
            "cancel" => {
                clear_pending(ctx);
                message.reply_html(ctx.tr("accounts.cancelled")).await?;
                Ok(())
            }
            "resend" => self.resend(ctx, message).await,
            "code" | "sms" => self.submit_code(ctx, message, args).await,
            "2fa" | "password" => self.submit_password(ctx, message, args).await,
            _ => self.start_login(ctx, message, action).await,
        }
    }

    async fn start_login(&self, ctx: &Arc<Ctx>, message: &Msg, raw_phone: &str) -> Result<()> {
        match start_login_core(ctx, raw_phone).await {
            Ok(html) | Err(html) => {
                message.reply_html(html).await?;
            }
        }
        Ok(())
    }

    async fn resend(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let phone = ctx.db.get_str(NS, "pending_phone", "");
        let hash = ctx.db.get_str(NS, "pending_hash", "");
        let id = ctx.db.get_str(NS, "pending_id", "");
        if phone.is_empty() || hash.is_empty() || id.is_empty() {
            message.reply_html(ctx.tr("accounts.no_pending")).await?;
            return Ok(());
        }

        let session_path = accounts::account_session_path(&ctx.data_root, &id);
        let one = OneShot::open(&ctx.config, &session_path).await?;
        let result = login::resend_code(&one.client, &phone, &hash, &ctx.translator.lang()).await;
        one.quit().await;

        match result {
            Ok(sent) => {
                ctx.db.set(NS, "pending_hash", sent.phone_code_hash.clone());
                let delivery = login::delivery_hint(&sent.r#type, &phone, &ctx.translator.lang());
                message
                    .reply_html(ctx.tf(
                        "accounts.code_resent",
                        &[&escape_html(&phone), &escape_html(&delivery)],
                    ))
                    .await?;
            }
            Err(err) => {
                message
                    .reply_html(ctx.tf("accounts.login_failed", &[&escape_html(&err.to_string())]))
                    .await?;
            }
        }
        Ok(())
    }

    async fn submit_code(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(code) = args.get(1) else {
            message.reply_html(ctx.tr("accounts.code_usage")).await?;
            return Ok(());
        };
        let html = match submit_code_core(ctx, code).await {
            AccountStep::Done(html)
            | AccountStep::NeedPassword(html)
            | AccountStep::NeedCode(html) => html,
        };
        message.reply_html(html).await?;
        Ok(())
    }

    async fn submit_password(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let password = args
            .list
            .get(1..)
            .map(|rest| rest.join(" "))
            .unwrap_or_default();
        if password.trim().is_empty() {
            message
                .reply_html(ctx.tr("accounts.password_usage"))
                .await?;
            return Ok(());
        }
        let html = match submit_password_core(ctx, &password).await {
            AccountStep::Done(html)
            | AccountStep::NeedPassword(html)
            | AccountStep::NeedCode(html) => html,
        };
        message.reply_html(html).await?;
        Ok(())
    }

    async fn finish(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        one: OneShot,
        phone: &str,
        id: &str,
    ) -> Result<()> {
        let html = match finish_account(ctx, one, phone, id).await {
            Ok(html) => html,
            Err(err) => ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            ),
        };
        message.reply_html(html).await?;
        Ok(())
    }

    async fn switch(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let registry = Registry::load(&ctx.data_root);
        let Some(raw) = args.get(0) else {
            let (html, buttons) = accounts_card(ctx);
            if ctx
                .inline
                .send_to_user(ctx, ctx.me_id, html, buttons)
                .await
                .is_err()
            {
                message
                    .reply_html(self.accounts_list(ctx, &registry))
                    .await?;
            }
            return Ok(());
        };

        let target = raw.trim();
        if target.eq_ignore_ascii_case("default")
            || target.eq_ignore_ascii_case("main")
            || target == "0"
        {
            let mut registry = registry;
            registry.active = None;
            registry.save(&ctx.data_root)?;
            ctx.db.save();
            message
                .reply_html(ctx.tr("accounts.switching_default"))
                .await?;
            schedule_restart();
            return Ok(());
        }

        let Some(account) = registry
            .get(target)
            .or_else(|| registry.get_by_phone(target))
            .cloned()
        else {
            message
                .reply_html(ctx.tf("accounts.not_found", &[&escape_html(target)]))
                .await?;
            return Ok(());
        };

        let session = accounts::account_session_path(&ctx.data_root, &account.id);
        if !session.exists() {
            message
                .reply_html(ctx.tf("accounts.session_missing", &[&escape_html(&account.id)]))
                .await?;
            return Ok(());
        }

        if account.tg_id == ctx.me_id {
            message
                .reply_html(ctx.tf("accounts.already_active", &[&escape_html(&account.name)]))
                .await?;
            return Ok(());
        }

        let mut registry = registry;
        registry.active = Some(account.id.clone());
        registry.save(&ctx.data_root)?;
        seed_account_db(ctx, &account.id, account.tg_id);
        ctx.db.save();
        message
            .reply_html(ctx.tf(
                "accounts.switching",
                &[&escape_html(&account.name), &escape_html(&account.id)],
            ))
            .await?;
        schedule_restart();
        Ok(())
    }

    fn accounts_list(&self, ctx: &Arc<Ctx>, registry: &Registry) -> String {
        let mut lines = vec![ctx.tr("accounts.title")];
        if registry.accounts.is_empty() {
            lines.push(ctx.tr("accounts.empty"));
            lines.push(String::new());
            lines.push(ctx.tr("accounts.add_hint"));
            return lines.join("\n");
        }

        lines.push(ctx.tr("accounts.default_line"));
        for account in &registry.accounts {
            let marker = if account.tg_id == ctx.me_id {
                "✅"
            } else if registry.active.as_deref() == Some(account.id.as_str()) {
                "🔘"
            } else {
                "▫️"
            };
            let username = account
                .username
                .as_deref()
                .map(|username| format!(" (@{username})"))
                .unwrap_or_default();
            lines.push(format!(
                "{marker} <code>{}</code> — <b>{}</b>{} · <i>{}</i>",
                escape_html(&account.id),
                escape_html(&account.name),
                escape_html(&username),
                escape_html(&account.phone)
            ));
        }
        lines.push(String::new());
        lines.push(ctx.tr("accounts.switch_hint"));
        lines.join("\n")
    }
}

/// Result of one step of the account login state machine. Every variant
/// carries ready-to-send HTML so both the userbot command and the inline-bot
/// dialog can render the same messages.
pub(crate) enum AccountStep {
    Done(String),
    NeedPassword(String),
    NeedCode(String),
}

/// One reply produced by the inline-bot account dialog.
pub(crate) struct BotReply {
    pub html: String,
    pub next: Option<PendingInput>,
}

pub(crate) async fn bot_account_step(
    ctx: &Arc<Ctx>,
    pending: &PendingInput,
    text: &str,
) -> BotReply {
    match pending {
        PendingInput::AccountPhone => match start_login_core(ctx, text).await {
            Ok(html) => BotReply {
                html,
                next: Some(PendingInput::AccountCode),
            },
            Err(html) => BotReply { html, next: None },
        },
        PendingInput::AccountCode => match submit_code_core(ctx, text).await {
            AccountStep::Done(html) => BotReply { html, next: None },
            AccountStep::NeedPassword(html) => BotReply {
                html,
                next: Some(PendingInput::AccountPassword),
            },
            AccountStep::NeedCode(html) => BotReply {
                html,
                next: Some(PendingInput::AccountCode),
            },
        },
        PendingInput::AccountPassword => match submit_password_core(ctx, text).await {
            AccountStep::Done(html) => BotReply { html, next: None },
            AccountStep::NeedPassword(html) | AccountStep::NeedCode(html) => BotReply {
                html,
                next: Some(PendingInput::AccountPassword),
            },
        },
        // Configuration and security prompts are handled before this function.
        _ => BotReply {
            html: ctx.tr("inline.expected_text"),
            next: None,
        },
    }
}

async fn start_login_core(ctx: &Arc<Ctx>, raw_phone: &str) -> std::result::Result<String, String> {
    let phone = login::normalize_phone(raw_phone)
        .map_err(|err| ctx.tf("accounts.bad_phone", &[&escape_html(&err.to_string())]))?;

    let id = accounts::account_id(&phone);
    let session_path = accounts::account_session_path(&ctx.data_root, &id);
    let one = OneShot::open(&ctx.config, &session_path)
        .await
        .map_err(|err| {
            ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            )
        })?;

    let sent = match login::send_code(
        &one.client,
        &phone,
        ctx.config.api_id,
        ctx.config.api_hash.as_str(),
        &ctx.translator.lang(),
    )
    .await
    {
        Ok(sent) => sent,
        Err(err) => {
            one.quit().await;
            return Err(ctx.tf("accounts.login_failed", &[&escape_html(&err.to_string())]));
        }
    };

    ctx.db.set(NS, "pending_phone", phone.clone());
    ctx.db.set(NS, "pending_hash", sent.phone_code_hash.clone());
    ctx.db.set(NS, "pending_id", id);
    ctx.db.set(NS, "pending_step", "code");
    one.quit().await;

    let delivery = login::delivery_hint(&sent.r#type, &phone, &ctx.translator.lang());
    Ok(ctx.tf(
        "accounts.code_sent",
        &[&escape_html(&phone), &escape_html(&delivery)],
    ))
}

async fn submit_code_core(ctx: &Arc<Ctx>, code: &str) -> AccountStep {
    let phone = ctx.db.get_str(NS, "pending_phone", "");
    let hash = ctx.db.get_str(NS, "pending_hash", "");
    let id = ctx.db.get_str(NS, "pending_id", "");
    if phone.is_empty() || hash.is_empty() || id.is_empty() {
        return AccountStep::NeedCode(ctx.tr("accounts.no_pending"));
    }

    let session_path = accounts::account_session_path(&ctx.data_root, &id);
    let one = match OneShot::open(&ctx.config, &session_path).await {
        Ok(one) => one,
        Err(err) => {
            return AccountStep::NeedCode(ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            ))
        }
    };

    match login::account_sign_in(&one.client, &one.session, &phone, &hash, code).await {
        Ok(login::CodeOutcome::SignedIn) => match finish_account(ctx, one, &phone, &id).await {
            Ok(html) => AccountStep::Done(html),
            Err(err) => AccountStep::NeedCode(ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            )),
        },
        Ok(login::CodeOutcome::PasswordRequired) => {
            one.quit().await;
            ctx.db.set(NS, "pending_step", "2fa");
            AccountStep::NeedPassword(ctx.tr("accounts.password_required"))
        }
        Err(err) => {
            one.quit().await;
            AccountStep::NeedCode(ctx.tf("accounts.code_failed", &[&escape_html(&err.to_string())]))
        }
    }
}

async fn submit_password_core(ctx: &Arc<Ctx>, password: &str) -> AccountStep {
    let phone = ctx.db.get_str(NS, "pending_phone", "");
    let id = ctx.db.get_str(NS, "pending_id", "");
    if phone.is_empty() || id.is_empty() {
        return AccountStep::NeedPassword(ctx.tr("accounts.no_pending"));
    }

    let session_path = accounts::account_session_path(&ctx.data_root, &id);
    let one = match OneShot::open(&ctx.config, &session_path).await {
        Ok(one) => one,
        Err(err) => {
            return AccountStep::NeedPassword(ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            ))
        }
    };

    match login::account_check_password(&one.client, &one.session, password).await {
        Ok(()) => match finish_account(ctx, one, &phone, &id).await {
            Ok(html) => AccountStep::Done(html),
            Err(err) => AccountStep::NeedPassword(ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            )),
        },
        Err(err) => {
            one.quit().await;
            AccountStep::NeedPassword(ctx.tf(
                "accounts.password_failed",
                &[&escape_html(&err.to_string())],
            ))
        }
    }
}

async fn finish_account(ctx: &Arc<Ctx>, one: OneShot, phone: &str, id: &str) -> Result<String> {
    let me = match one.client.get_me().await {
        Ok(me) => me,
        Err(err) => {
            one.quit().await;
            return Err(err).context("failed to fetch the new account profile");
        }
    };

    let account = Account {
        id: id.to_owned(),
        phone: phone.to_owned(),
        name: me.full_name(),
        username: me.username().map(ToOwned::to_owned),
        tg_id: me.id().bot_api_dialog_id_unchecked(),
    };
    one.quit().await;

    let mut registry = Registry::load(&ctx.data_root);
    registry.upsert(account.clone());
    registry.save(&ctx.data_root)?;
    seed_account_db(ctx, &account.id, account.tg_id);
    clear_pending(ctx);

    Ok(ctx.tf(
        "accounts.added",
        &[
            &escape_html(&account.name),
            &escape_html(&account.phone),
            &escape_html(&account.id),
        ],
    ))
}

pub(crate) fn accounts_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let registry = Registry::load(&ctx.data_root);
    let mut lines = vec![ctx.tr("accounts.title")];

    if registry.accounts.is_empty() {
        lines.push(ctx.tr("accounts.empty"));
        lines.push(String::new());
        lines.push(ctx.tr("accounts.add_hint"));
    } else {
        lines.push(ctx.tr("accounts.default_line"));
        for account in &registry.accounts {
            let marker = if account.tg_id == ctx.me_id {
                "✅"
            } else if registry.active.as_deref() == Some(account.id.as_str()) {
                "🔘"
            } else {
                "▫️"
            };
            let username = account
                .username
                .as_deref()
                .map(|username| format!(" (@{username})"))
                .unwrap_or_default();
            lines.push(format!(
                "{marker} <code>{}</code> — <b>{}</b>{} · <i>{}</i>",
                escape_html(&account.id),
                escape_html(&account.name),
                escape_html(&username),
                escape_html(&account.phone)
            ));
        }
        lines.push(String::new());
        lines.push(ctx.tr("accounts.switch_hint"));
    }

    let mut rows = Vec::new();
    rows.push(vec![Button::data(
        ctx.tr("accounts.btn_default"),
        b"heroku:acc:default".to_vec(),
    )]);
    for account in &registry.accounts {
        rows.push(vec![Button::data(
            format!("👤 {}", account.name),
            format!("heroku:acc:sw:{}", account.id).into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("accounts.btn_add"),
        b"heroku:acc:add".to_vec(),
    )]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

/// Switch the active account from an inline callback and return the card text.
pub(crate) async fn switch_inline(ctx: &Arc<Ctx>, raw: &str) -> String {
    let registry = Registry::load(&ctx.data_root);
    let target = raw.trim();

    if target.eq_ignore_ascii_case("default")
        || target.eq_ignore_ascii_case("main")
        || target == "0"
    {
        let mut registry = registry;
        registry.active = None;
        if let Err(err) = registry.save(&ctx.data_root) {
            return ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            );
        }
        ctx.db.save();
        schedule_restart();
        return ctx.tr("accounts.switching_default");
    }

    let Some(account) = registry
        .get(target)
        .or_else(|| registry.get_by_phone(target))
        .cloned()
    else {
        return ctx.tf("accounts.not_found", &[&escape_html(target)]);
    };

    let session = accounts::account_session_path(&ctx.data_root, &account.id);
    if !session.exists() {
        return ctx.tf("accounts.session_missing", &[&escape_html(&account.id)]);
    }
    if account.tg_id == ctx.me_id {
        return ctx.tf("accounts.already_active", &[&escape_html(&account.name)]);
    }

    let mut registry = registry;
    registry.active = Some(account.id.clone());
    if let Err(err) = registry.save(&ctx.data_root) {
        return ctx.tf(
            "accounts.login_failed",
            &[&escape_html(&format!("{err:#}"))],
        );
    }
    seed_account_db(ctx, &account.id, account.tg_id);
    ctx.db.save();
    schedule_restart();
    ctx.tf(
        "accounts.switching",
        &[&escape_html(&account.name), &escape_html(&account.id)],
    )
}

pub(crate) async fn resend_inline(ctx: &Arc<Ctx>) -> String {
    let phone = ctx.db.get_str(NS, "pending_phone", "");
    let hash = ctx.db.get_str(NS, "pending_hash", "");
    let id = ctx.db.get_str(NS, "pending_id", "");
    if phone.is_empty() || hash.is_empty() || id.is_empty() {
        return ctx.tr("accounts.no_pending");
    }

    let session_path = accounts::account_session_path(&ctx.data_root, &id);
    let one = match OneShot::open(&ctx.config, &session_path).await {
        Ok(one) => one,
        Err(err) => {
            return ctx.tf(
                "accounts.login_failed",
                &[&escape_html(&format!("{err:#}"))],
            )
        }
    };
    let result = login::resend_code(&one.client, &phone, &hash, &ctx.translator.lang()).await;
    one.quit().await;

    match result {
        Ok(sent) => {
            ctx.db.set(NS, "pending_hash", sent.phone_code_hash.clone());
            let delivery = login::delivery_hint(&sent.r#type, &phone, &ctx.translator.lang());
            ctx.tf(
                "accounts.code_resent",
                &[&escape_html(&phone), &escape_html(&delivery)],
            )
        }
        Err(err) => ctx.tf("accounts.login_failed", &[&escape_html(&err.to_string())]),
    }
}

struct OneShot {
    client: Client,
    session: Arc<ErasedSession>,
    handle: SenderPoolFatHandle,
    task: JoinHandle<()>,
}

impl OneShot {
    async fn open(config: &Config, session_path: &Path) -> Result<Self> {
        if let Some(parent) = session_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let session = Arc::new(
            SqliteSession::open(session_path)
                .await
                .context("failed to open the account session")?,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(session_path, std::fs::Permissions::from_mode(0o600));
        }
        let session_view = erase(session.clone());
        let SenderPool { runner, handle, .. } = SenderPool::new(session, config.api_id);
        let task = tokio::spawn(runner.run());
        let client = Client::new(handle.clone());
        Ok(Self {
            client,
            session: session_view,
            handle,
            task,
        })
    }

    async fn quit(self) {
        self.handle.quit();
        let _ = self.task.await;
    }
}

fn clear_pending(ctx: &Arc<Ctx>) {
    for key in [
        "pending_phone",
        "pending_hash",
        "pending_id",
        "pending_step",
    ] {
        ctx.db.remove(NS, key);
    }
}

/// Give a newly added account the same settings as the current one, matching
/// Heroku's `_clone_switch_db` behaviour. The session stays account-specific.
fn seed_account_db(ctx: &Arc<Ctx>, id: &str, tg_id: i64) {
    let path = accounts::account_db_path(&ctx.data_root, id);
    let Ok(target) = crate::database::Database::open(&path) else {
        return;
    };
    target.restore(ctx.db.dump());

    // The previous owner keeps their old account; the new account owns itself.
    let mut owners = target.get_i64_list(crate::security::NS, "owner");
    owners.retain(|owner| *owner != ctx.me_id);
    if tg_id != 0 && !owners.contains(&tg_id) {
        owners.push(tg_id);
    }
    target.set_value(
        crate::security::NS,
        "owner",
        serde_json::Value::Array(owners.into_iter().map(serde_json::Value::from).collect()),
    );

    // Never reuse the inline bot token across accounts.
    for key in ["bot_token", "custom_bot", "bot_id"] {
        target.remove("heroku.inline", key);
    }
    for key in [
        "pending_phone",
        "pending_hash",
        "pending_id",
        "pending_step",
    ] {
        target.remove(NS, key);
    }
    target.save();
}

fn schedule_restart() {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        if let Err(err) = crate::inline::restart_process() {
            error!("Account switch restart failed: {err}");
        }
    });
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuAccounts))
}
