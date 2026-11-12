// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.info` — userbot information.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct HerokuInfoMod;

#[async_trait]
impl Module for HerokuInfoMod {
    fn name(&self) -> &'static str {
        "HerokuInfoMod"
    }

    fn description(&self) -> &'static str {
        "Show userbot information"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("info", "Show userbot information"),
            CommandDef::new("ubinfo", "Show userbot information"),
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
            .reply_html(format!(
                "{title}\n\n\
                 {version}\n\
                 {uptime}\n\
                 {owner}\n\
                 {user}\n\
                 {modules}\n\
                 {engine}\n\
                 {platform}\n\n\
                 <i>🌐 <a href=\"https://github.com/Heroku-userbot/Heroku\">GitHub</a></i>",
                title = ctx.tr("info.title"),
                version = ctx.tf("info.version", &[crate::config::VERSION]),
                uptime = ctx.tf("info.uptime", &[&ctx.uptime()]),
                owner = ctx.tf("info.owner", &[&ctx.me_id.to_string()]),
                user = ctx.tf("info.user", &[&escape_html(&ctx.me_name)]),
                modules = ctx.tf("info.modules", &[&ctx.modules.all().len().to_string()]),
                engine = ctx.tr("info.python"),
                platform = ctx.tf(
                    "info.platform",
                    &[&format!(
                        "{} / {}",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    )]
                ),
            ))
            .await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuInfoMod))
}
