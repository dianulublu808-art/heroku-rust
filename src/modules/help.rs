// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.help` — module and command documentation.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
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

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "core_emoji",
                "Emoji for core modules in the help list",
                ConfigDefault::Text("🪐"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "plain_emoji",
                "Emoji for regular modules in the help list",
                ConfigDefault::Text("▫️"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "empty_emoji",
                "Emoji for modules without commands",
                ConfigDefault::Text("🟠"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "desc_icon",
                "Icon shown before the help header",
                ConfigDefault::Text("🪐"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "command_emoji",
                "Emoji shown before commands",
                ConfigDefault::Text("▫️"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "banner_url",
                "Banner shown above the help list",
                ConfigDefault::Text(""),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "media_quote",
                "Show the help banner as a link preview",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "invert_media",
                "Show the help banner below the text",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "show_preview_in_help",
                "Show module banners in module help",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "rich_mode",
                "Render help as Telegram rich messages",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
        ]
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
        // Prefer the interactive inline cover, exactly like Python's gallery.
        let user_id = message.sender_id().unwrap_or(ctx.me_id);
        if ctx.inline.send_help_page(ctx, user_id, 0).await.is_ok() {
            return Ok(());
        }

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
                ctx.db.get_str("Help", "empty_emoji", "🟠")
            } else {
                ctx.db.get_str("Help", "plain_emoji", "▫️")
            };
            body.push_str(&format!(
                "{bullet} <b>{}</b> <code>[{}]</code>\n",
                escape_html(short_name(module.name())),
                commands.len()
            ));
        }

        let prefix = prefix(ctx, message);
        let footer = ctx.tf(
            "help.all_header",
            &[
                &ctx.modules.all().len().to_string(),
                &hidden.len().to_string(),
            ],
        );
        let hint = ctx.tf("help.view_hint", &[&format!("{prefix}help <module>")]);
        message
            .reply_html(format!(
                "{title}\n<i>{subtitle}</i>\n\n{body}\n<i>{footer}</i>\n\n{hint}",
                title = ctx.tr("help.title"),
                subtitle = ctx.tf("help.subtitle", &[crate::config::VERSION]),
            ))
            .await?;
        Ok(())
    }

    async fn show(&self, ctx: &Arc<Ctx>, message: &Msg, query: &str) -> Result<()> {
        let query = query.trim().trim_start_matches('.');
        let prefix = prefix(ctx, message);

        if let Some(module) = ctx.modules.get(query) {
            let user_id = message.sender_id().unwrap_or(ctx.me_id);
            if ctx
                .inline
                .send_help_module(ctx, user_id, module.name())
                .await
                .is_ok()
            {
                return Ok(());
            }

            let commands = module.commands();
            let mut body = String::new();
            for command in commands {
                body.push_str(&ctx.tf("help.command", &[&format!("{prefix}{}", command.name)]));
                if !command.usage.is_empty() {
                    body.push_str(&format!(" <code>{}</code>", escape_html(command.usage)));
                }
                if !command.description.is_empty() {
                    body.push_str(&format!("\n   <i>{}</i>", escape_html(command.description)));
                }
                if !command.aliases.is_empty() {
                    let aliases = command
                        .aliases
                        .iter()
                        .map(|alias| format!("{prefix}{alias}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    body.push_str(&format!(
                        "\n   ↳ <i>{label}: {aliases}</i>",
                        label = ctx.tr("help.aliases")
                    ));
                }
                body.push('\n');
            }
            if body.is_empty() {
                body = ctx.tr("help.no_commands");
            }
            message
                .reply_html(format!(
                    "{title}{description}\n\n{body}\n<i>👤 {author} · v{version}</i>",
                    title = ctx.tf("help.module", &[&escape_html(module.name())]),
                    description = if module.description().is_empty() {
                        String::new()
                    } else {
                        format!("\n<i>{}</i>", escape_html(module.description()))
                    },
                    author = escape_html(module.author()),
                    version = escape_html(module.version()),
                ))
                .await?;
            return Ok(());
        }

        let matches = ctx.modules.find_command(query);
        if let Some((module, command)) = matches.last() {
            let usage = if command.usage.is_empty() {
                format!("{prefix}{}", command.name)
            } else {
                format!("{prefix}{} {}", command.name, command.usage)
            };
            let mut text = format!(
                "{title}{description}\n\n▫️ {usage_label}: <code>{usage}</code>\n▫️ {module_label}: <b>{module}</b>",
                title = ctx.tf("help.command", &[&format!("{prefix}{}", command.name)]),
                description = if command.description.is_empty() {
                    String::new()
                } else {
                    format!("\n<i>{}</i>", escape_html(command.description))
                },
                usage_label = ctx.tr("help.usage"),
                usage = escape_html(&usage),
                module_label = ctx.tr("help.module_label"),
                module = escape_html(short_name(module.name())),
            );
            if !command.aliases.is_empty() {
                let aliases = command
                    .aliases
                    .iter()
                    .map(|alias| format!("{prefix}{alias}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                text.push_str(&format!(
                    "\n▫️ {}: <code>{aliases}</code>",
                    ctx.tr("help.aliases")
                ));
            }
            message.reply_html(text).await?;
            return Ok(());
        }

        if let Some(suggestions) = self.suggest(ctx, query) {
            message
                .reply_html(ctx.tf("help.suggestions", &[&suggestions]))
                .await?;
            return Ok(());
        }

        message.reply_html(ctx.tr("help.no_module")).await?;
        Ok(())
    }

    /// Fuzzy fallback for typos: match module and command names/descriptions.
    fn suggest(&self, ctx: &Arc<Ctx>, query: &str) -> Option<String> {
        let needle = query.to_lowercase();
        if needle.len() < 2 {
            return None;
        }

        let mut lines: Vec<String> = Vec::new();
        for module in ctx.modules.all() {
            if module.name().to_lowercase().contains(&needle)
                || module.description().to_lowercase().contains(&needle)
            {
                lines.push(format!(
                    "▫️ <b>.help {}</b> — <i>{}</i>",
                    escape_html(short_name(module.name())),
                    escape_html(module.description())
                ));
            }
            for command in module.commands() {
                if command.name.to_lowercase().contains(&needle) {
                    lines.push(format!(
                        "▫️ <code>.help {}</code> — <i>{}</i>",
                        escape_html(command.name),
                        escape_html(command.description)
                    ));
                }
            }
            if lines.len() >= 10 {
                break;
            }
        }

        if lines.is_empty() {
            None
        } else {
            lines.truncate(10);
            Some(lines.join("\n"))
        }
    }

    async fn hide(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if args.list.is_empty() {
            message.reply_html(ctx.tr("help.no_mod")).await?;
            return Ok(());
        }
        let mut hidden = ctx.db.get_string_list("Help", "hide");
        let mut newly_hidden = Vec::new();
        let mut shown = Vec::new();
        for name in &args.list {
            let Some(module) = ctx.modules.get(name) else {
                continue;
            };
            if let Some(index) = hidden
                .iter()
                .position(|item| item.eq_ignore_ascii_case(module.name()))
            {
                hidden.remove(index);
                shown.push(module.name().to_owned());
            } else {
                hidden.push(module.name().to_owned());
                newly_hidden.push(module.name().to_owned());
            }
        }
        ctx.db.set(
            "Help",
            "hide",
            serde_json::Value::Array(hidden.iter().cloned().map(Into::into).collect()),
        );
        let hidden_lines = hidden
            .iter()
            .map(|name| format!("👁‍🗨 <i>{}</i>", escape_html(name)))
            .collect::<Vec<_>>()
            .join("\n");
        let shown_lines = shown
            .iter()
            .map(|name| format!("👁 <i>{}</i>", escape_html(name)))
            .collect::<Vec<_>>()
            .join("\n");
        message
            .reply_html(ctx.tf(
                "help.hidden_shown",
                &[
                    &newly_hidden.len().to_string(),
                    &shown.len().to_string(),
                    &hidden_lines,
                    &shown_lines,
                ],
            ))
            .await?;
        Ok(())
    }
}

/// `CoreMod` → `Core`, `Help` → `Help`.
fn short_name(name: &str) -> &str {
    name.strip_suffix("Mod")
        .filter(|name| !name.is_empty())
        .unwrap_or(name)
}

fn prefix(ctx: &Ctx, message: &Msg) -> String {
    let user_id = message.sender_id().unwrap_or(ctx.me_id);
    ctx.prefixes(user_id)
        .into_iter()
        .next()
        .unwrap_or_else(|| ".".to_owned())
}

inventory::submit! {
    ModuleFactory(|| Box::new(Help))
}

#[cfg(test)]
mod tests {
    use super::short_name;

    #[test]
    fn shortens_module_names() {
        assert_eq!(short_name("CoreMod"), "Core");
        assert_eq!(short_name("HerokuInfoMod"), "HerokuInfo");
        assert_eq!(short_name("Help"), "Help");
        assert_eq!(short_name("Mod"), "Mod");
    }
}
