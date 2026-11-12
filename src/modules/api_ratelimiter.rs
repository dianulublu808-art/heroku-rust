// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! API rate limiter.
//!
//! grammers and Telegram itself already throttle requests. This module exists so
//! that the command namespace and database layout stay compatible with the
//! Python build.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::Msg;

pub struct APIRatelimiterMod;

#[async_trait]
impl Module for APIRatelimiterMod {
    fn name(&self) -> &'static str {
        "APIRatelimiterMod"
    }

    fn description(&self) -> &'static str {
        "Telegram API rate limiting status"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("apilimiter", "Show API rate limiter status"),
            CommandDef::new("setapilimiter", "Configure API rate limiting").usage("<value>"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        message
            .reply_html(
                "🚦 <b>API rate limiter</b>\n\
                 <i>Requests are throttled by grammers and Telegram flood-wait handling.</i>",
            )
            .await?;
        let _ = ctx;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(APIRatelimiterMod))
}
