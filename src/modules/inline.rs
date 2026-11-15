// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `InlineStuff` — inline bot management, status and the auto-delete watcher.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::inline::INLINE_NS;
use crate::module::{CommandDef, Module, ModuleFactory, OWNER};
use crate::security::NS as SECURITY_NS;
use crate::tg::{escape_html, Msg};

pub struct InlineStuff;

#[async_trait]
impl Module for InlineStuff {
    fn name(&self) -> &'static str {
        "InlineStuff"
    }

    fn description(&self) -> &'static str {
        "Inline bot, search, callbacks and bot PM"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("inline", "Show the inline bot status"),
            CommandDef::new("inlinestatus", "Show the inline bot status"),
            CommandDef::new("ch_bot_token", "Set the inline bot token").usage("<token>"),
            CommandDef::new("ch_heroku_bot", "Change the inline bot username")
                .aliases(&["chbot"])
                .usage("[@username]"),
            CommandDef::new("inlinesec", "Show inline command security")
                .usage("[command]")
                .security(OWNER),
            CommandDef::new("querysec", "Allow inline queries for external users").security(OWNER),
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
            "inline" | "inlinestatus" => self.status(ctx, message).await,
            "ch_bot_token" => self.set_token(ctx, message, args).await,
            "ch_heroku_bot" => self.set_username(ctx, message, args).await,
            "inlinesec" => self.inline_security(ctx, message, args).await,
            "querysec" => self.query_security(ctx, message).await,
            _ => Ok(()),
        }
    }

    /// Delete service messages that our inline bot sends to the userbot.
    async fn on_message(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<bool> {
        if !message.outgoing() {
            return Ok(false);
        }
        let Some(bot_id) = ctx.inline.bot_id().await else {
            return Ok(false);
        };
        if message.inner.via_bot_id() != Some(bot_id) {
            return Ok(false);
        }

        let text = message.text();
        if text.contains("This message will be deleted automatically")
            || text.contains("Opening gallery...")
        {
            message.delete().await?;
            return Ok(true);
        }

        Ok(false)
    }
}

impl InlineStuff {
    async fn status(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let status = ctx.inline.snapshot().await;
        let mut out = format!("{}\n\n", ctx.tr("inline.status_title"));
        if status.running {
            out.push_str(
                &ctx.tn(
                    "inline.status_running",
                    &[
                        (
                            "username",
                            &escape_html(status.username.as_deref().unwrap_or("bot")),
                        ),
                        (
                            "id",
                            &status
                                .bot_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "—".into()),
                        ),
                    ],
                ),
            );
            if let Some(uptime) = ctx.inline.uptime().await {
                out.push_str(&format!(
                    "\n▫️ {}: <b>{}</b>",
                    ctx.tr("inline.uptime"),
                    format_duration(uptime)
                ));
            }
            let modules = ctx.modules.all().len();
            let commands: usize = ctx
                .modules
                .all()
                .iter()
                .map(|module| module.commands().len())
                .sum();
            out.push_str(&format!(
                "\n▫️ {}: <b>{}</b>\n▫️ {}: <b>{}</b>",
                ctx.tr("inline.modules"),
                modules,
                ctx.tr("inline.commands"),
                commands
            ));
        } else if status.configured {
            out.push_str(&ctx.tr("inline.status_stopped"));
        } else {
            out.push_str(&ctx.tr("inline.status_no_token"));
        }

        if let Some(error) = status.error {
            out.push_str(&format!(
                "\n\n{}",
                ctx.tf("inline.status_error", &[&escape_html(&error)])
            ));
        }

        if let Some(username) = ctx.inline.bot_username().await {
            out.push_str(&format!(
                "\n\n▫️ {}: <code>@{}</code>",
                ctx.tr("inline.bot"),
                escape_html(&username)
            ));
        }

        out.push_str(&format!(
            "\n\n<i>{}</i>",
            ctx.tf("inline.status_hint", &[&escape_html(&prefix(ctx))])
        ));

        message.reply_html(out).await?;
        Ok(())
    }

    async fn set_token(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let token = args.raw.trim();
        if token.is_empty() {
            message.reply_html(ctx.tr("inline.token_usage")).await?;
            return Ok(());
        }

        let token_re = Regex::new(r"^[0-9]{6,12}:[A-Za-z0-9_-]{20,}$").expect("valid token regex");
        if !token_re.is_match(token) {
            message.reply_html(ctx.tr("inline.token_invalid")).await?;
            return Ok(());
        }

        ctx.db
            .set(INLINE_NS, "bot_token", Value::String(token.to_owned()));
        ctx.db
            .set(INLINE_NS, "needs_inline_setup", Value::Bool(true));
        message.reply_html(ctx.tr("inline.token_saved")).await?;

        match ctx.inline.restart(ctx).await {
            Ok(()) => {
                if let Some(username) = ctx.inline.bot_username().await {
                    message
                        .reply_html(ctx.tf("inline.token_started", &[&escape_html(&username)]))
                        .await?;
                }
            }
            Err(err) => {
                message
                    .reply_html(ctx.tf(
                        "inline.token_start_failed",
                        &[&escape_html(&format!("{err:#}"))],
                    ))
                    .await?;
            }
        }
        Ok(())
    }

    async fn set_username(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let raw = args.raw.trim().trim_start_matches('@');
        if raw.is_empty() {
            let current = ctx
                .db
                .get_str(INLINE_NS, "bot_username", "")
                .trim()
                .to_owned();
            let current = if current.is_empty() {
                ctx.inline.bot_username().await.unwrap_or_default()
            } else {
                current
            };
            let mut out = if current.is_empty() {
                ctx.tr("inline.bot_username_none")
            } else {
                ctx.tf("inline.bot_username_current", &[&escape_html(&current)])
            };
            out.push_str("\n\n");
            out.push_str(&ctx.tr("inline.bot_username_help"));
            message.reply_html(out).await?;
            return Ok(());
        }

        if raw.len() <= 4
            || !raw.to_lowercase().ends_with("bot")
            || !raw
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            message
                .reply_html(ctx.tr("inline.bot_username_invalid"))
                .await?;
            return Ok(());
        }

        if ctx.resolve(raw).await.is_some() {
            message
                .reply_html(ctx.tr("inline.bot_username_occupied"))
                .await?;
            return Ok(());
        }

        ctx.db
            .set(INLINE_NS, "custom_bot", Value::String(raw.to_owned()));
        ctx.db.remove(INLINE_NS, "bot_token");
        ctx.inline.stop().await;
        message
            .reply_html(ctx.tf("inline.bot_updated", &[&escape_html(raw)]))
            .await?;
        Ok(())
    }

    async fn inline_security(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let raw = args.raw.trim().trim_start_matches('.');
        if raw.is_empty() {
            let allow = ctx.db.get_bool(SECURITY_NS, "allow_inline_query", false);
            let state = if allow {
                ctx.tr("inline.querysec_enabled")
            } else {
                ctx.tr("inline.querysec_disabled")
            };
            message
                .reply_html(ctx.tf("inline.inlinesec_global", &[&state]))
                .await?;
            return Ok(());
        }

        let command = raw.split_whitespace().next().unwrap_or_default();
        let Some((module, def)) = ctx.modules.find_command(command).into_iter().next() else {
            message.reply_html(ctx.tr("common.not_found")).await?;
            return Ok(());
        };
        let mask = ctx
            .security
            .command_mask(module.name(), def.name, def.security);
        let names = crate::security::mask_to_names(mask);
        let names = if names.is_empty() {
            "—".to_owned()
        } else {
            names.join(", ")
        };
        message
            .reply_html(ctx.tn(
                "inline.inlinesec_command",
                &[
                    ("command", &escape_html(def.name)),
                    ("module", &escape_html(module.name())),
                    ("mask", &mask.to_string()),
                    ("names", &escape_html(&names)),
                ],
            ))
            .await?;
        Ok(())
    }

    async fn query_security(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let current = ctx.db.get_bool(SECURITY_NS, "allow_inline_query", false);
        ctx.db
            .set(SECURITY_NS, "allow_inline_query", Value::Bool(!current));
        let key = if current {
            "inline.querysec_off"
        } else {
            "inline.querysec_on"
        };
        message.reply_html(ctx.tr(key)).await?;
        Ok(())
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

fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs();
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

inventory::submit! {
    ModuleFactory(|| Box::new(InlineStuff))
}
