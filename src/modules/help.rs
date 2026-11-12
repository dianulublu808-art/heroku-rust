// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.help` — module and command documentation.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct Help;

#[async_trait]
impl Module for Help {
    fn name(&self) -> &'static str {
        "Help"
    }

    fn description(&self) -> &'static str {
        "Shows help for modules and commands"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("help", "Show the list of modules or help for one of them")
                .aliases(&["h"])
                .usage("[module|command]"),
            CommandDef::new("modhelp", "Show help for a module").usage("<module>"),
            CommandDef::new("support", "Show support links"),
            CommandDef::new("helphide", "Hide modules from the help list").usage("<module...>"),
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
            "support" => {
                message.reply_html(ctx.tr("help.support")).await?;
            }
            "helphide" => self.hide(ctx, message, args).await?,
            _ => {
                let query = args.raw.trim();
                if query.is_empty() {
                    self.list(ctx, message).await?;
                } else {
                    self.show(ctx, message, query).await?;
                }
            }
        }
        Ok(())
    }
}

impl Help {
    async fn list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let hidden = ctx.db.get_string_list("Help", "hide");
        let mut body = String::new();
        for module in ctx.modules.all() {
            if hidden
                .iter()
                .any(|name| name.eq_ignore_ascii_case(module.name()))
            {
                continue;
            }
            let commands = module.commands();
            let bullet = if commands.is_empty() {
                "🟠"
            } else {
                "▪️"
            };
            body.push_str(&format!(
                "{bullet} <b>{}</b> <code>[{}]</code>\n",
                escape_html(module.name()),
                commands.len()
            ));
        }
        let footer = ctx.tf(
            "help.hidden_shown",
            &[
                &hidden.len().to_string(),
                &(ctx.modules.all().len().saturating_sub(hidden.len())).to_string(),
            ],
        );
        message
            .reply_html(format!(
                "{}\n\n{body}\n<i>{footer}</i>\n\n▫️ <code>.help &lt;module&gt;</code> — details",
                ctx.tr("help.title")
            ))
            .await?;
        Ok(())
    }

    async fn show(&self, ctx: &Arc<Ctx>, message: &Msg, query: &str) -> Result<()> {
        let query = query.trim().trim_start_matches('.');

        if let Some(module) = ctx.modules.get(query) {
            let commands = module.commands();
            let mut body = String::new();
            for command in commands {
                body.push_str(&format!(
                    "{} <code>.{}</code>",
                    ctx.tf("help.command", &[&escape_html(&command.name)]),
                    escape_html(&command.usage)
                ));
                if !command.usage.is_empty() {
                    body.push_str(&format!(" <code>{}</code>", escape_html(command.usage)));
                }
                if !command.description.is_empty() {
                    body.push_str(&format!("\n   <i>{}</i>", escape_html(command.description)));
                }
                body.push('\n');
            }
            if body.is_empty() {
                body = ctx.tr("help.no_commands");
            }
            message
                .reply_html(format!(
                    "{}\n<i>{}</i>\n\n{body}",
                    ctx.tf("help.module", &[&escape_html(module.name())]),
                    escape_html(module.description())
                ))
                .await?;
            return Ok(());
        }

        let matches = ctx.modules.find_command(query);
        if let Some((module, command)) = matches.last() {
            let usage = if command.usage.is_empty() {
                format!(".{}", command.name)
            } else {
                format!(".{} {}", command.name, command.usage)
            };
            let mut text = format!(
                "{}\n<i>{}</i>\n\n▫️ {}: <code>{}</code>\n▫️ <i>{}</i>",
                ctx.tf("help.command", &[&escape_html(&command.name)]),
                escape_html(&module.name()),
                ctx.tr("help.usage"),
                escape_html(&usage),
                escape_html(command.description)
            );
            if !command.aliases.is_empty() {
                let aliases = command
                    .aliases
                    .iter()
                    .map(|alias| format!(".{alias}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                text.push_str(&format!("\n▫️ <i>{aliases}</i>"));
            }
            message.reply_html(text).await?;
            return Ok(());
        }

        message.reply_html(ctx.tr("help.no_module")).await?;
        Ok(())
    }

    async fn hide(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if args.list.is_empty() {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        }
        let mut hidden = ctx.db.get_string_list("Help", "hide");
        for name in &args.list {
            let Some(module) = ctx.modules.get(name) else {
                continue;
            };
            if let Some(index) = hidden
                .iter()
                .position(|item| item.eq_ignore_ascii_case(module.name()))
            {
                hidden.remove(index);
            } else {
                hidden.push(module.name().to_owned());
            }
        }
        ctx.db.set(
            "Help",
            "hide",
            serde_json::Value::Array(hidden.iter().cloned().map(Into::into).collect()),
        );
        message
            .reply_html(
                hidden
                    .iter()
                    .map(|name| format!("👁‍🗨 <i>{}</i>", escape_html(name)))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .await?;
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(Help))
}
