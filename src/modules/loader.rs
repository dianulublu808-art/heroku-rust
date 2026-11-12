// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Loader placeholders.
//!
//! The Python build downloads and imports external modules at runtime. The Rust
//! build is statically linked, so these commands answer honestly instead of
//! pretending the feature exists.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct LoaderMod;

#[async_trait]
impl Module for LoaderMod {
    fn name(&self) -> &'static str {
        "LoaderMod"
    }

    fn description(&self) -> &'static str {
        "Load, unload and manage modules (static build stub)"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("loadmod", "Load an external module").usage("<module>"),
            CommandDef::new("unloadmod", "Unload a module").usage("<module>"),
            CommandDef::new("reloadmod", "Reload a module").usage("<module>"),
            CommandDef::new("dlmod", "Download a module from a URL").usage("<url>"),
            CommandDef::new("loadall", "Load all modules"),
            CommandDef::new("unloadall", "Unload all modules"),
            CommandDef::new("listmods", "List all modules"),
            CommandDef::new("modinfo", "Show information about a module").usage("<module>"),
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
            "listmods" => {
                let mut lines = vec!["🧩 <b>Modules</b>".to_owned()];
                for module in ctx.modules.all() {
                    lines.push(format!(
                        "▪️ <code>{}</code> — <i>{}</i>",
                        escape_html(module.name()),
                        escape_html(module.description())
                    ));
                }
                message.reply_html(lines.join("\n")).await?;
            }
            "modinfo" => {
                let Some(name) = args.get(0) else {
                    message.reply_html(ctx.tr("common.no_args")).await?;
                    return Ok(());
                };
                let Some(module) = ctx.modules.get(name) else {
                    message.reply_html(ctx.tr("help.no_module")).await?;
                    return Ok(());
                };
                message
                    .reply_html(format!(
                        "🧩 <b>{}</b>\n<i>{}</i>\n\n▫️ {} <code>{}</code>\n▫️ {} <code>{}</code>",
                        escape_html(module.name()),
                        escape_html(module.description()),
                        ctx.tr("help.commands"),
                        module.commands().len(),
                        ctx.tr("help.author"),
                        escape_html(module.author())
                    ))
                    .await?;
            }
            _ => {
                message.reply_html(ctx.tr("loader.unavailable")).await?;
            }
        }
        Ok(())
    }
}

pub struct LoaderRestrictor;

#[async_trait]
impl Module for LoaderRestrictor {
    fn name(&self) -> &'static str {
        "LoaderRestrictor"
    }

    fn description(&self) -> &'static str {
        "Restrict module loading (static build stub)"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("allowmod", "Allow a module to be loaded").usage("<module>"),
            CommandDef::new("denymod", "Deny a module from being loaded").usage("<module>"),
            CommandDef::new("allowedmods", "List allowed modules"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        message.reply_html(ctx.tr("loader.unavailable")).await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(LoaderMod))
}

inventory::submit! {
    ModuleFactory(|| Box::new(LoaderRestrictor))
}
