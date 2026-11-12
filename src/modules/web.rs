// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Web panel placeholder.
//!
//! The original web panel is a FastAPI application bundled with the Python
//! build. It is tracked in the roadmap separately from the userbot core.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::Msg;

pub struct HerokuWebMod;

#[async_trait]
impl Module for HerokuWebMod {
    fn name(&self) -> &'static str {
        "HerokuWebMod"
    }

    fn description(&self) -> &'static str {
        "Web panel (not available in the Rust build)"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("web", "Show web panel status"),
            CommandDef::new("webbind", "Change the web panel bind address").usage("<host:port>"),
            CommandDef::new("websecret", "Show the web panel secret"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        message.reply_html(ctx.tr("web.unavailable")).await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuWebMod))
}
