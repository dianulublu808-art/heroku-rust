// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Core settings: prefix, aliases, blacklists and module toggles.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::{Button, InputMessage, InputRichMessage};
use serde_json::{json, Value};

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

const HEROKU_PHOTO: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/heroku_cmd.jpg";

pub struct CoreMod;

#[async_trait]
impl Module for CoreMod {
    fn name(&self) -> &'static str {
        "CoreMod"
    }

    fn description(&self) -> &'static str {
        "Control core userbot settings"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("heroku", "Show information about Heroku"),
            CommandDef::new("blacklist", "Blacklist a chat or a module in a chat")
                .aliases(&["bl"])
                .usage("[chat_id] [module]"),
            CommandDef::new("unblacklist", "Remove a chat or module from the blacklist")
                .aliases(&["unbl"])
                .usage("[chat_id] [module]"),
            CommandDef::new("blacklistuser", "Blacklist a user").usage("<user>"),
            CommandDef::new("unblacklistuser", "Unblacklist a user").usage("<user>"),
            CommandDef::new("setprefix", "Set command prefixes").usage("<prefix...>"),
            CommandDef::new("aliases", "List configured aliases"),
            CommandDef::new("addalias", "Create a command alias").usage("<alias> <command> [args]"),
            CommandDef::new("delalias", "Delete an alias").usage("<alias>|-c"),
            CommandDef::new("cleardb", "Clear the whole database").usage("--force"),
            CommandDef::new("togglecmd", "Enable or disable a command").usage("[module] <command>"),
            CommandDef::new("togglemod", "Enable or disable a module").usage("<module>"),
            CommandDef::new("clearmodule", "Remove all data of a module").usage("<module>"),
            CommandDef::new("installation", "Installation guide").usage("[-vds|-wsl|-ul|-hh|-uni]"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "allow_nonstandart_prefixes",
                "Allow prefixes that are not punctuation characters",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "alias_emoji",
                "Emoji shown next to aliases in listings",
                ConfigDefault::Text("▪️"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "rich_mode",
                "Use rich formatting where the client supports it",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "keep_message_state",
                "Remember message state for reaction handling",
                ConfigDefault::Bool(true),
                ConfigKind::Bool,
            ),
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
            "heroku" => self.heroku(ctx, message).await,
            "blacklist" => self.toggle_blacklist(ctx, message, args, true).await,
            "unblacklist" => self.toggle_blacklist(ctx, message, args, false).await,
            "blacklistuser" => self.toggle_blacklist_user(ctx, message, args, true).await,
            "unblacklistuser" => self.toggle_blacklist_user(ctx, message, args, false).await,
            "setprefix" => self.set_prefix(ctx, message, args).await,
            "aliases" => self.aliases(ctx, message).await,
            "addalias" => self.add_alias(ctx, message, args).await,
            "delalias" => self.del_alias(ctx, message, args).await,
            "cleardb" => self.clear_db(ctx, message, args).await,
            "togglecmd" => self.toggle_command(ctx, message, args).await,
            "togglemod" => self.toggle_module(ctx, message, args).await,
            "clearmodule" => self.clear_module(ctx, message, args).await,
            "installation" => self.installation(ctx, message, args).await,
            _ => Ok(()),
        }
    }
}

impl CoreMod {
    async fn heroku(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let parts: Vec<&str> = crate::config::VERSION.split('.').collect();
        let major = parts.first().copied().unwrap_or("0").to_owned();
        let minor = parts.get(1).copied().unwrap_or("0").to_owned();
        let patch = parts.get(2).copied().unwrap_or("0").to_owned();
        let build = crate::modules::heroku_info::commit_url().await;
        let htl = format!("grammers 0.12.0 #{}", grammers_tl_types::LAYER);

        let branch = crate::modules::heroku_info::branch().await;
        let branch_text = match branch.as_str() {
            "" | "main" | "master" | "Unknown" => String::new(),
            "beta" => ctx.tf("settings.happy_beta", &[&branch]),
            other => ctx.tf("settings.unstable", &[other]),
        };

        if ctx.db.get_bool("CoreMod", "rich_mode", false) {
            let mut rich = ctx.tn(
                "settings.rich_heroku_message",
                &[
                    ("platform", &crate::modules::heroku_info::platform_emoji()),
                    ("version", crate::config::VERSION),
                    ("build", &build),
                    ("htl_version", "grammers 0.12.0"),
                    ("layer", &grammers_tl_types::LAYER.to_string()),
                    ("current_user", &crate::modules::heroku_info::current_user()),
                    ("banner_url", HEROKU_PHOTO),
                ],
            );
            if !branch_text.is_empty() {
                rich.push_str(&format!("<footer>{}</footer>", branch_text.trim()));
            }
            crate::protection::guard().await;
            let input = InputMessage::new().rich_message(InputRichMessage::html(rich));
            message.inner.reply(input).await?;
            return Ok(());
        }

        let mut html = ctx.tf(
            "settings.heroku",
            &[
                "🪐 <b>Heroku userbot</b>",
                &major,
                &minor,
                &patch,
                &build,
                &htl,
            ],
        );
        html.push_str(&branch_text);

        crate::protection::guard().await;
        let input = InputMessage::new()
            .html(html.clone())
            .photo_url(HEROKU_PHOTO.to_owned());
        match message.inner.reply(input).await {
            Ok(_) => Ok(()),
            Err(err) => {
                tracing::warn!("Failed to send the Heroku banner: {err}");
                message.reply_html(html).await?;
                Ok(())
            }
        }
    }

    fn blacklist_entry(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> String {
        let mut chat_id = crate::modules::common::blacklist_chat_id(message.chat_id());
        let mut module: Option<String> = None;
        for value in &args.list {
            match value.parse::<i64>() {
                Ok(id) => chat_id = crate::modules::common::blacklist_chat_id(id),
                Err(_) => {
                    if let Some(found) = ctx.modules.get(value) {
                        module = Some(found.name().to_owned());
                    }
                }
            }
        }
        match module {
            Some(module) => format!("{chat_id}.{module}"),
            None => chat_id.to_string(),
        }
    }

    async fn toggle_blacklist(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        args: &Args,
        add: bool,
    ) -> Result<()> {
        let entry = Self::blacklist_entry(ctx, message, args);
        let mut list = ctx.db.get_string_list("heroku.main", "blacklist_chats");
        if add {
            if !list.contains(&entry) {
                list.push(entry.clone());
            }
        } else {
            list.retain(|item| item != &entry);
        }
        ctx.db.set(
            "heroku.main",
            "blacklist_chats",
            Value::Array(list.into_iter().map(Value::String).collect()),
        );
        let key = if add {
            "settings.blacklisted"
        } else {
            "settings.unblacklisted"
        };
        message
            .reply_html(ctx.tf(key, &[&escape_html(&entry)]))
            .await?;
        Ok(())
    }

    async fn toggle_blacklist_user(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        args: &Args,
        add: bool,
    ) -> Result<()> {
        let Some(user) = ctx
            .target_user(message, args.raw.trim())
            .await
            .map(|peer| super::common::peer_id(&peer))
        else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        let mut list = ctx.db.get_i64_list("heroku.main", "blacklist_users");
        if add {
            if !list.contains(&user) {
                list.push(user);
            }
        } else {
            list.retain(|id| id != &user);
        }
        ctx.db.set(
            "heroku.main",
            "blacklist_users",
            Value::Array(list.into_iter().map(Value::from).collect()),
        );
        message
            .reply_html(ctx.tf(
                if add {
                    "settings.user_blacklisted"
                } else {
                    "settings.user_unblacklisted"
                },
                &[&user.to_string()],
            ))
            .await?;
        Ok(())
    }

    async fn set_prefix(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let mut prefixes = args.list.clone();
        if prefixes.is_empty() {
            message.reply_html(ctx.tr("settings.what_prefix")).await?;
            return Ok(());
        }

        let mut target = message.sender_id().unwrap_or(ctx.me_id);
        if let Some(user) = args.value("user") {
            if let Ok(id) = user.parse::<i64>() {
                target = id;
            }
        } else if prefixes.len() > 1 {
            let last = prefixes.last().cloned().unwrap_or_default();
            if last.starts_with('@')
                || (last.chars().all(|ch| ch.is_ascii_digit()) && last.len() >= 5)
            {
                prefixes.pop();
                if let Some(peer) = ctx.resolve(&last).await {
                    target = super::common::peer_id(&peer);
                }
            }
        }

        if prefixes.iter().any(|prefix| {
            prefix.is_empty() || prefix == "s" || prefix.chars().any(char::is_whitespace)
        }) {
            message.reply_html(ctx.tr("settings.what_prefix")).await?;
            return Ok(());
        }

        if target == ctx.me_id {
            ctx.db
                .set("heroku.main", "command_prefix", prefixes[0].as_str());
            ctx.db.set(
                "heroku.main",
                "command_prefix_aliases",
                Value::Array(prefixes[1..].iter().cloned().map(Value::String).collect()),
            );
        } else {
            let mut personal = ctx.db.get_map("heroku.main", "command_prefixes");
            let value = if prefixes.len() == 1 {
                Value::String(prefixes[0].clone())
            } else {
                Value::Array(prefixes.iter().cloned().map(Value::String).collect())
            };
            personal.insert(target.to_string(), value);
            ctx.db
                .set("heroku.main", "command_prefixes", Value::Object(personal));
        }

        let joined = escape_html(&prefixes.join(" "));
        message
            .reply_html(ctx.tf("settings.prefix_set", &[&joined]))
            .await?;
        Ok(())
    }

    async fn aliases(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let aliases = ctx.db.get_map("CoreMod", "aliases");
        if aliases.is_empty() {
            message.reply_html(ctx.tr("settings.aliases_empty")).await?;
            return Ok(());
        }
        let mut lines = vec![ctx.tr("settings.aliases")];
        for (alias, target) in aliases {
            lines.push(format!(
                "▪️ <code>{}</code> → <code>{}</code>",
                escape_html(&alias),
                escape_html(target.as_str().unwrap_or_default())
            ));
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn add_alias(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let raw = args.raw.trim();
        let Some((alias, rest)) = raw.split_once(char::is_whitespace) else {
            message.reply_html(ctx.tr("settings.alias_args")).await?;
            return Ok(());
        };
        let (command, command_args) = super::common::split_once(rest.trim());
        let command = command.trim_start_matches('.');
        if command.is_empty() || ctx.modules.find_command(command).is_empty() {
            message
                .reply_html(ctx.tr("settings.command_not_found"))
                .await?;
            return Ok(());
        }
        let target = if command_args.is_empty() {
            command.to_owned()
        } else {
            format!("{command} {command_args}")
        };
        let mut aliases = ctx.db.get_map("CoreMod", "aliases");
        for alias in alias.split(',').map(str::trim).filter(|a| !a.is_empty()) {
            aliases.insert(alias.to_lowercase(), Value::String(target.clone()));
            message
                .reply_html(ctx.tf(
                    "settings.alias_added",
                    &[&escape_html(alias), &escape_html(&target)],
                ))
                .await?;
        }
        ctx.db.set("CoreMod", "aliases", Value::Object(aliases));
        Ok(())
    }

    async fn del_alias(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let mut aliases = ctx.db.get_map("CoreMod", "aliases");
        if args.flag("c") || args.flag("clear") || args.raw.trim() == "-c" {
            aliases.clear();
            ctx.db.set("CoreMod", "aliases", Value::Object(aliases));
            message
                .reply_html(ctx.tr("settings.alias_removed").replace("{}", "*"))
                .await?;
            return Ok(());
        }
        let alias = args.raw.trim().trim_start_matches('.');
        if alias.is_empty() {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        }
        if aliases.remove(alias).is_none() {
            message.reply_html(ctx.tr("settings.alias_missing")).await?;
            return Ok(());
        }
        ctx.db.set("CoreMod", "aliases", Value::Object(aliases));
        message
            .reply_html(ctx.tf("settings.alias_removed", &[&escape_html(alias)]))
            .await?;
        Ok(())
    }

    async fn clear_db(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if !args.flag("force") && !args.flag("f") {
            message
                .reply_html(ctx.tr("settings.cleardb_confirm"))
                .await?;
            return Ok(());
        }
        ctx.db.restore(json!({}));
        message.reply_html(ctx.tr("settings.cleardb_done")).await?;
        Ok(())
    }

    async fn toggle_command(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let (module, command) = match args.list.as_slice() {
            [module, command, ..] => {
                let Some(found) = ctx.modules.get(module) else {
                    message
                        .reply_html(ctx.tr("settings.module_not_found"))
                        .await?;
                    return Ok(());
                };
                (found, command.clone())
            }
            [command] => {
                let Some((found, def)) = ctx.modules.find_command(command).into_iter().next()
                else {
                    message
                        .reply_html(ctx.tr("settings.command_not_found"))
                        .await?;
                    return Ok(());
                };
                (found, def.name.to_owned())
            }
            _ => {
                message.reply_html(ctx.tr("common.no_args")).await?;
                return Ok(());
            }
        };

        if ctx.modules.find_command(&command).is_empty() {
            message
                .reply_html(ctx.tr("settings.command_not_found"))
                .await?;
            return Ok(());
        }

        let mut disabled = ctx.db.get_map("heroku.main", "disabled_commands");
        let mut commands: Vec<String> = disabled
            .get(module.name())
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let enabled = if let Some(index) = commands
            .iter()
            .position(|item| item.eq_ignore_ascii_case(&command))
        {
            commands.remove(index);
            true
        } else {
            commands.push(command.clone());
            false
        };

        if commands.is_empty() {
            disabled.remove(module.name());
        } else {
            disabled.insert(
                module.name().to_owned(),
                Value::Array(commands.into_iter().map(Value::String).collect()),
            );
        }
        ctx.db
            .set("heroku.main", "disabled_commands", Value::Object(disabled));

        message
            .reply_html(ctx.tf(
                if enabled {
                    "settings.toggled_on"
                } else {
                    "settings.toggled_off"
                },
                &[&format!(
                    "{}.{}",
                    escape_html(module.name()),
                    escape_html(&command)
                )],
            ))
            .await?;
        Ok(())
    }

    async fn toggle_module(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let Some(module) = ctx.modules.get(name) else {
            message
                .reply_html(ctx.tr("settings.module_not_found"))
                .await?;
            return Ok(());
        };
        let mut disabled = ctx.db.get_string_list("heroku.main", "disabled_modules");
        let enabled = if let Some(index) = disabled
            .iter()
            .position(|item| item.eq_ignore_ascii_case(module.name()))
        {
            disabled.remove(index);
            true
        } else {
            disabled.push(module.name().to_owned());
            false
        };
        ctx.db.set(
            "heroku.main",
            "disabled_modules",
            Value::Array(disabled.into_iter().map(Value::String).collect()),
        );
        message
            .reply_html(ctx.tf(
                if enabled {
                    "settings.toggled_on"
                } else {
                    "settings.toggled_off"
                },
                &[&escape_html(module.name())],
            ))
            .await?;
        Ok(())
    }

    async fn clear_module(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let key = ctx
            .modules
            .get(name)
            .map(|module| module.name().to_owned())
            .unwrap_or_else(|| name.to_owned());
        ctx.db.clear_owner(&key);

        let mut disabled = ctx.db.get_map("heroku.main", "disabled_commands");
        disabled.remove(&key);
        ctx.db
            .set("heroku.main", "disabled_commands", Value::Object(disabled));

        let mut modules = ctx.db.get_string_list("heroku.main", "disabled_modules");
        modules.retain(|item| !item.eq_ignore_ascii_case(&key));
        ctx.db.set(
            "heroku.main",
            "disabled_modules",
            Value::Array(modules.into_iter().map(Value::String).collect()),
        );

        message
            .reply_html(ctx.tf("settings.module_cleared", &[&escape_html(&key)]))
            .await?;
        Ok(())
    }

    async fn installation(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if let Some(platform) = installation_platform(&args.raw) {
            message.reply_html(installation_text(ctx, platform)).await?;
            return Ok(());
        }

        let (html, buttons) = installation_card(ctx);
        if ctx
            .inline
            .send_photo_to_user(ctx, ctx.me_id, INSTALL_PHOTO, html, buttons)
            .await
            .is_ok()
        {
            return Ok(());
        }

        // Without the inline bot the picker is unusable; Python falls back to
        // the installation photo with the VDS guide as its caption.
        let guide = ctx.tr("settings.vds_install");
        crate::protection::guard().await;
        let input = InputMessage::new()
            .photo_url(INSTALL_PHOTO.to_owned())
            .html(guide.clone());
        if message.inner.reply(input).await.is_err() {
            message.reply_html(guide).await?;
        }
        Ok(())
    }
}

const INSTALL_PHOTO: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/heroku_installation.jpg";

const INSTALL_PLATFORMS: &[&str] = &["vds", "wsl", "userland", "hikkahost", "universal"];

/// Map Python's installation flags to a platform key.
fn installation_platform(raw: &str) -> Option<&'static str> {
    for token in raw.split_whitespace() {
        let platform = match token {
            "-vds" => "vds",
            "-wsl" => "wsl",
            "-ul" | "-userland" => "userland",
            "-hh" | "-hikkahost" => "hikkahost",
            "-uni" | "-universal" => "universal",
            _ => continue,
        };
        return Some(platform);
    }
    None
}

fn installation_text(ctx: &Arc<Ctx>, platform: &str) -> String {
    ctx.tr(&format!("settings.{platform}_install"))
}

/// Inline installation picker: caption plus platform buttons, two per row.
pub(crate) fn installation_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for platform in INSTALL_PLATFORMS {
        row.push(Button::data(
            ctx.tr(&format!("settings.{platform}")),
            format!("heroku:install:{platform}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    (ctx.tr("settings.choose_installation"), rows)
}

inventory::submit! {
    ModuleFactory(|| Box::new(CoreMod))
}
