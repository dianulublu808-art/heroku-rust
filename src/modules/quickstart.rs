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
        if ctx.db.get_bool("Quickstart", "sent", false) {
            return Ok(());
        }
        ctx.send_self(ctx.tr("quickstart.text")).await?;
        ctx.db.set("Quickstart", "sent", true);
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
