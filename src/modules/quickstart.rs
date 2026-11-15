// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Sends the welcome message on the first run.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::Msg;

const JOIN_REASON: &str = "Heroku help is only available in this chat. By agreeing to join the chat, you agree to the Heroku federation rules and if you violate them, you will be permanently banned.";

pub struct Quickstart;

#[async_trait]
impl Module for Quickstart {
    fn name(&self) -> &'static str {
        "Quickstart"
    }

    fn description(&self) -> &'static str {
        "Show a short guide on the first launch"
    }

    fn commands(&self) -> Vec<CommandDef> {
        Vec::new()
    }

    async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
        // Prepare the content channel and the Assets/Backups forum topics in the
        // background, exactly like Python's `client_ready` hook.
        let content_ctx = ctx.clone();
        tokio::spawn(async move {
            if let Err(err) = crate::content::ensure_topics(&content_ctx).await {
                tracing::warn!("Failed to prepare the content channel: {err:#}");
            }
        });

        // Python's `client_ready` also asks the owner to join the support chat.
        let join_ctx = ctx.clone();
        tokio::spawn(async move {
            match crate::content::request_join(&join_ctx, crate::content::SUPPORT_CHAT, JOIN_REASON)
                .await
            {
                Ok(_) => {}
                Err(err) => tracing::warn!("Failed to request joining the support chat: {err:#}"),
            }
        });

        // Python stores the flag as `no_msg`; honour it so a migrated database
        // does not show the welcome card again.
        let already_sent = ctx.db.get_bool("Quickstart", "sent", false)
            || ctx.db.get_bool("Quickstart", "no_msg", false);
        if already_sent {
            return Ok(());
        }

        // Prefer the bot card with the language picker (Python sends it from
        // `inline_ready`). If no inline bot is configured or the bot cannot PM
        // the owner yet, fall back to a normal userbot message.
        if ctx.inline.send_welcome(ctx, ctx.me_id).await.is_ok() {
            ctx.db.set("Quickstart", "sent", true);
            ctx.db.set("Quickstart", "no_msg", true);
            return Ok(());
        }

        ctx.send_self(ctx.tf("quickstart.base", &["Heroku"]))
            .await?;
        ctx.db.set("Quickstart", "sent", true);
        ctx.db.set("Quickstart", "no_msg", true);
        Ok(())
    }

    async fn handle(
        &self,
        _ctx: &Arc<Ctx>,
        _command: &str,
        _message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(Quickstart))
}
