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
        let owner_id = ctx.security.owners().first().copied().unwrap_or(ctx.me_id);
        let owner = if owner_id == ctx.me_id {
            format!(
                "<a href=\"tg://user?id={owner_id}\">{}</a>",
                escape_html(&ctx.me_name)
            )
        } else {
            format!("<a href=\"tg://user?id={owner_id}\">{owner_id}</a>")
        };
        let account = ctx
            .me_username
            .as_ref()
            .map(|username| format!("@{username}"))
            .unwrap_or_else(|| escape_html(&ctx.me_name));
        let prefix = ctx
            .prefixes(ctx.me_id)
            .into_iter()
            .next()
            .unwrap_or_else(|| ".".to_owned());

        message
            .reply_html(format!(
                "{title}\n<i>{subtitle}</i>\n\n\
                 {version}\n\
                 {uptime}\n\
                 {owner}\n\
                 {account}\n\
                 {modules}\n\
                 {prefix_line}\n\n\
                 {engine}\n\
                 {platform}\n\n\
                 {links}",
                title = ctx.tr("info.title"),
                subtitle = ctx.tr("info.subtitle"),
                version = ctx.tf("info.version", &[crate::config::VERSION]),
                uptime = ctx.tf("info.uptime", &[&ctx.uptime()]),
                owner = ctx.tf("info.owner", &[&owner]),
                account = ctx.tf("info.user", &[&account]),
                modules = ctx.tf("info.modules", &[&ctx.modules.all().len().to_string()]),
                prefix_line = ctx.tf("info.prefix", &[&escape_html(&prefix)]),
                engine = ctx.tr("info.python"),
                platform = ctx.tf(
                    "info.platform",
                    &[&format!(
                        "{} / {}",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    )]
                ),
                links = ctx.tr("info.links"),
            ))
            .await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuInfoMod))
}
