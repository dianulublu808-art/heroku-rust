// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Inline bot placeholder.
//!
//! Inline queries require a bot account, a separate connection and a webhook or
//! long-polling loop. That is tracked in the roadmap and not part of the core
//! rewrite yet.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::Msg;

pub struct InlineStuff;

#[async_trait]
impl Module for InlineStuff {
    fn name(&self) -> &'static str {
        "InlineStuff"
    }

    fn description(&self) -> &'static str {
        "Inline bot handlers (not available in the Rust build)"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("inline", "Show inline bot status"),
            CommandDef::new("inlinestatus", "Show inline bot status"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        message.reply_html(ctx.tr("inline.unavailable")).await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(InlineStuff))
}
