// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! External API flood protection.
//!
//! This is the real Heroku `APILimiter`: every outgoing message action is
//! counted in a sliding window (see [`crate::protection`]) and the userbot is
//! frozen locally when the threshold is exceeded. The commands below let the
//! owner inspect the state, toggle protection and suspend it temporarily.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::protection::protection;
use crate::tg::{escape_html, Msg};

/// Database namespace used by [`APIRatelimiterMod`]. Matches the module name so
/// `.config APIRatelimiterMod` edits the same values in real time.
pub const NS: &str = "APIRatelimiterMod";

pub struct APIRatelimiterMod;

#[async_trait]
impl Module for APIRatelimiterMod {
    fn name(&self) -> &'static str {
        "APIRatelimiterMod"
    }

    fn description(&self) -> &'static str {
        "Helps the userbot avoid spamming the Telegram API"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new(
                "api_fw_protection",
                "Show or toggle local Telegram API flood protection",
            )
            .usage("[on|off]"),
            CommandDef::new(
                "suspend_api_protect",
                "Temporarily disable API flood protection",
            )
            .usage("<seconds>"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "enabled",
                "Enable local API flood protection",
                ConfigDefault::Bool(true),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "time_sample",
                "Time sample (seconds) used to count outgoing requests",
                ConfigDefault::Int(15),
                ConfigKind::Int,
            ),
            ConfigSpec::new(
                "threshold",
                "Number of requests within the sample that triggers protection",
                ConfigDefault::Int(100),
                ConfigKind::Int,
            ),
            ConfigSpec::new(
                "local_floodwait",
                "Freeze the userbot for this many seconds when the limit is hit",
                ConfigDefault::Int(30),
                ConfigKind::Int,
            ),
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
            "suspend_api_protect" => self.suspend(ctx, message, args).await,
            _ => self.toggle(ctx, message, args).await,
        }
    }
}

impl APIRatelimiterMod {
    async fn suspend(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(raw) = args.get(0) else {
            message
                .reply_html(ctx.tr("api_protection.args_invalid"))
                .await?;
            return Ok(());
        };

        let Ok(seconds) = raw.trim().parse::<u64>() else {
            message
                .reply_html(ctx.tr("api_protection.args_invalid"))
                .await?;
            return Ok(());
        };

        if seconds == 0 {
            message
                .reply_html(ctx.tr("api_protection.args_invalid"))
                .await?;
            return Ok(());
        }

        let seconds = seconds.min(86_400 * 365);
        protection().suspend(seconds);
        message
            .reply_html(ctx.tf("api_protection.suspended_for", &[&seconds.to_string()]))
            .await?;
        Ok(())
    }

    async fn toggle(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let state = protection();
        if let Some(raw) = args.get(0) {
            let enabled = match raw.trim().to_lowercase().as_str() {
                "on" | "enable" | "true" | "1" => true,
                "off" | "disable" | "false" | "0" => false,
                _ => {
                    message
                        .reply_html(ctx.tr("api_protection.args_invalid"))
                        .await?;
                    return Ok(());
                }
            };
            ctx.db.set(NS, "enabled", enabled);
            state.set_enabled(enabled);
            let key = if enabled {
                "api_protection.on"
            } else {
                "api_protection.off"
            };
            message.reply_html(ctx.tr(key)).await?;
            return Ok(());
        }

        let suspended = state
            .suspended_for()
            .map(|seconds| format!("{seconds}s"))
            .unwrap_or_else(|| "—".to_owned());
        let frozen = state
            .frozen_for()
            .map(|seconds| format!("{seconds}s"))
            .unwrap_or_else(|| "—".to_owned());

        let text = ctx.tn(
            "api_protection.status",
            &[
                ("enabled", if state.enabled() { "✅" } else { "❌" }),
                ("threshold", &state.threshold().to_string()),
                ("sample", &state.sample_secs().to_string()),
                ("freeze", &state.freeze_secs().to_string()),
                ("frozen", &escape_html(&frozen)),
                ("suspended", &escape_html(&suspended)),
            ],
        );
        message.reply_html(text).await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(APIRatelimiterMod))
}
