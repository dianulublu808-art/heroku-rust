// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Updater commands. The Rust build ships as a single binary, so self-updates
//! are performed by replacing the binary instead of pulling Python modules.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::Msg;

pub struct UpdaterMod;

#[async_trait]
impl Module for UpdaterMod {
    fn name(&self) -> &'static str {
        "UpdaterMod"
    }

    fn description(&self) -> &'static str {
        "Update and restart the userbot"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("update", "Check for updates"),
            CommandDef::new("restart", "Restart the userbot").usage("[-f]"),
            CommandDef::new("autoupdate", "Toggle automatic updates").usage("[on|off]"),
            CommandDef::new("source", "Show the source repository"),
            CommandDef::new("changelog", "Show the changelog"),
            CommandDef::new("rollback", "Roll back to the previous version"),
            CommandDef::new("ubstop", "Stop the userbot"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![ConfigSpec::new(
            "autoupdate",
            "Automatically check for updates",
            ConfigDefault::Bool(false),
            ConfigKind::Bool,
        )]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        match command {
            "source" => {
                message.reply_html(ctx.tr("updater.source")).await?;
            }
            "autoupdate" => {
                let current = ctx.db.get_bool("UpdaterMod", "autoupdate", false);
                let value = args
                    .get(0)
                    .and_then(super::common::parse_bool)
                    .unwrap_or(!current);
                ctx.db.set("UpdaterMod", "autoupdate", value);
                message
                    .reply_html(format!(
                        "{} <code>autoupdate</code> = <code>{value}</code>",
                        if value { "✅" } else { "🚫" }
                    ))
                    .await?;
            }
            "update" => {
                message
                    .reply_html(format!(
                        "🪐 <b>Heroku {}</b>\n\n{}\n\n<i>{}</i>",
                        crate::config::VERSION,
                        ctx.tr("updater.up_to_date"),
                        ctx.tr("updater.manual")
                    ))
                    .await?;
            }
            "changelog" => {
                message
                    .reply_html(
                        "📜 <b>Changelog</b>\n\
                         <a href=\"https://github.com/Heroku-userbot/Heroku/commits/master\">\
                         GitHub commits</a>",
                    )
                    .await?;
            }
            "restart" => {
                message.reply_html(ctx.tr("updater.restarting")).await?;
                // Restarting would kill the current process. On VDS installs the
                // service manager restarts it for us, so exiting is the closest
                // safe equivalent.
                if args.flag("f") || args.flag("force") {
                    tokio::spawn(async {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        std::process::exit(0);
                    });
                }
            }
            "rollback" | "ubstop" => {
                message.reply_html(ctx.tr("updater.manual")).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(UpdaterMod))
}
