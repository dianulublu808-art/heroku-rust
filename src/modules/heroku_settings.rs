// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Global userbot switches and nickname rules.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::Button;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct HerokuSettingsMod;

const TOGGLES: &[(&str, &str, bool)] = &[
    (
        "no_nickname",
        "Do not require the userbot username in groups",
        false,
    ),
    ("grep", "Enable | grep filtering for command output", false),
    (
        "inlinelogs",
        "Show exception details in command errors",
        true,
    ),
    (
        "keep_message_state",
        "Keep message state for reaction handling",
        true,
    ),
    (
        "suggest_subscribe",
        "Suggest subscribing to module author channels",
        true,
    ),
];

#[async_trait]
impl Module for HerokuSettingsMod {
    fn name(&self) -> &'static str {
        "HerokuSettingsMod"
    }

    fn description(&self) -> &'static str {
        "Manage global userbot settings and watchers"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("settings", "Show or change global settings").usage("[key] [on|off]"),
            CommandDef::new("watchers", "List modules with watchers"),
            CommandDef::new("watcher", "Toggle a module watcher").usage("<module>"),
            CommandDef::new("watcherbl", "Show disabled watchers"),
            CommandDef::new("nonickuser", "Toggle nickname exemption for a user").usage("<user>"),
            CommandDef::new(
                "nonickchat",
                "Toggle nickname exemption for the current chat",
            ),
            CommandDef::new("nonickcmd", "Toggle nickname exemption for a command")
                .usage("<command>"),
            CommandDef::new("nonickusers", "List nickname-exempt users"),
            CommandDef::new("nonickchats", "List nickname-exempt chats"),
            CommandDef::new("nonickcmds", "List nickname-exempt commands"),
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
            "settings" => self.settings(ctx, message, args).await,
            "watchers" => self.watchers(ctx, message).await,
            "watcher" => self.toggle_watcher(ctx, message, args).await,
            "watcherbl" => self.watcher_blacklist(ctx, message).await,
            "nonickuser" => self.nonick_user(ctx, message, args).await,
            "nonickchat" => self.nonick_chat(ctx, message).await,
            "nonickcmd" => self.nonick_command(ctx, message, args).await,
            "nonickusers" => {
                let users = ctx.db.get_i64_list("heroku.main", "nonickusers");
                message
                    .reply_html(format!(
                        "👤 <b>Nickname-exempt users</b>\n{}",
                        if users.is_empty() {
                            "▫️ None".to_owned()
                        } else {
                            users
                                .iter()
                                .map(|id| format!("▪️ <code>{id}</code>"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    ))
                    .await?;
                Ok(())
            }
            "nonickchats" => {
                let chats = ctx.db.get_i64_list("heroku.main", "nonickchats");
                message
                    .reply_html(format!(
                        "💬 <b>Nickname-exempt chats</b>\n{}",
                        if chats.is_empty() {
                            "▫️ None".to_owned()
                        } else {
                            chats
                                .iter()
                                .map(|id| format!("▪️ <code>{id}</code>"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    ))
                    .await?;
                Ok(())
            }
            "nonickcmds" => {
                let commands = ctx.db.get_string_list("heroku.main", "nonickcmds");
                message
                    .reply_html(format!(
                        "⚡️ <b>Nickname-exempt commands</b>\n{}",
                        if commands.is_empty() {
                            "▫️ None".to_owned()
                        } else {
                            commands
                                .iter()
                                .map(|command| format!("▪️ <code>.{}</code>", escape_html(command)))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    ))
                    .await?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl HerokuSettingsMod {
    async fn settings(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if args.get(0).is_none() {
            let (html, buttons) = settings_card(ctx);
            let user_id = message.sender_id().unwrap_or(ctx.me_id);
            if ctx
                .inline
                .send_to_user(ctx, user_id, html, buttons)
                .await
                .is_ok()
            {
                return Ok(());
            }
        }

        if let Some(key) = args.get(0) {
            if !TOGGLES.iter().any(|(name, _, _)| *name == key) {
                message.reply_html(ctx.tr("config.not_found")).await?;
                return Ok(());
            }
            if let Some(raw) = args.get(1) {
                let Some(value) = super::common::parse_bool(raw) else {
                    message.reply_html(ctx.tr("config.invalid")).await?;
                    return Ok(());
                };
                ctx.db.set("heroku.main", key, value);
            }
        }

        let mut lines = vec![ctx.tr("settings.title")];
        for (key, description, default) in TOGGLES {
            let enabled = ctx.db.get_bool("heroku.main", key, *default);
            lines.push(format!(
                "{} <code>{}</code> — <i>{}</i>",
                if enabled { "✅" } else { "🚫" },
                key,
                description
            ));
        }
        lines.push(String::new());
        lines.push("<i>Change with <code>.settings &lt;key&gt; on|off</code></i>".to_owned());
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn watchers(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let mut lines = vec!["👁 <b>Watchers</b>".to_owned()];
        let disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        for module in ctx
            .modules
            .all()
            .iter()
            .filter(|module| module.has_watcher())
        {
            let state = if disabled.contains_key(module.name()) {
                "🚫"
            } else {
                "✅"
            };
            lines.push(format!(
                "{state} <code>{}</code>",
                escape_html(module.name())
            ));
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn toggle_watcher(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
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
        if !module.has_watcher() {
            message
                .reply_html(ctx.tr("settings.module_not_found"))
                .await?;
            return Ok(());
        }
        let mut disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        let enabled = if disabled.remove(module.name()).is_some() {
            true
        } else {
            disabled.insert(module.name().to_owned(), Value::Array(vec!["*".into()]));
            false
        };
        ctx.db
            .set("heroku.main", "disabled_watchers", Value::Object(disabled));
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

    async fn watcher_blacklist(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        if disabled.is_empty() {
            message.reply_html("▫️ <b>No disabled watchers</b>").await?;
            return Ok(());
        }
        let lines = disabled
            .iter()
            .map(|(name, rules)| format!("🚫 <code>{}</code> — {}", escape_html(name), rules))
            .collect::<Vec<_>>()
            .join("\n");
        message
            .reply_html(format!("👁 <b>Disabled watchers</b>\n{lines}"))
            .await?;
        Ok(())
    }

    async fn nonick_user(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(user) = ctx
            .target_user(message, args.raw.trim())
            .await
            .map(|peer| super::common::peer_id(&peer))
        else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        let mut users = ctx.db.get_i64_list("heroku.main", "nonickusers");
        let enabled = if let Some(index) = users.iter().position(|id| *id == user) {
            users.remove(index);
            false
        } else {
            users.push(user);
            true
        };
        ctx.db.set(
            "heroku.main",
            "nonickusers",
            Value::Array(users.into_iter().map(Value::from).collect()),
        );
        message
            .reply_html(ctx.tf(
                if enabled {
                    "settings.toggled_on"
                } else {
                    "settings.toggled_off"
                },
                &[&user.to_string()],
            ))
            .await?;
        Ok(())
    }

    async fn nonick_chat(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let chat_id = message.chat_id();
        let mut chats = ctx.db.get_i64_list("heroku.main", "nonickchats");
        let enabled = if let Some(index) = chats.iter().position(|id| *id == chat_id) {
            chats.remove(index);
            false
        } else {
            chats.push(chat_id);
            true
        };
        ctx.db.set(
            "heroku.main",
            "nonickchats",
            Value::Array(chats.into_iter().map(Value::from).collect()),
        );
        message
            .reply_html(ctx.tf(
                if enabled {
                    "settings.toggled_on"
                } else {
                    "settings.toggled_off"
                },
                &[&chat_id.to_string()],
            ))
            .await?;
        Ok(())
    }

    async fn nonick_command(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let name = name.trim_start_matches('.');
        let mut commands = ctx.db.get_string_list("heroku.main", "nonickcmds");
        let enabled = if let Some(index) = commands
            .iter()
            .position(|command| command.eq_ignore_ascii_case(name))
        {
            commands.remove(index);
            false
        } else {
            commands.push(name.to_owned());
            true
        };
        ctx.db.set(
            "heroku.main",
            "nonickcmds",
            Value::Array(commands.into_iter().map(Value::String).collect()),
        );
        message
            .reply_html(ctx.tf(
                if enabled {
                    "settings.toggled_on"
                } else {
                    "settings.toggled_off"
                },
                &[&escape_html(name)],
            ))
            .await?;
        Ok(())
    }
}

pub(crate) fn settings_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let mut lines = vec![ctx.tr("settings.title")];
    for (key, description, default) in TOGGLES {
        let enabled = ctx.db.get_bool("heroku.main", key, *default);
        lines.push(format!(
            "{} <code>{}</code> — <i>{}</i>",
            if enabled { "✅" } else { "🚫" },
            key,
            description
        ));
    }
    lines.push(String::new());
    lines.push(ctx.tr("settings.inline_hint"));

    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (key, _, default) in TOGGLES {
        let enabled = ctx.db.get_bool("heroku.main", key, *default);
        row.push(Button::data(
            format!("{} {key}", if enabled { "✅" } else { "🚫" }),
            format!("heroku:set:{key}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![
        Button::data("🔄 Restart", b"heroku:upd:restart".to_vec()),
        Button::data("🪂 Update", b"heroku:upd:apply".to_vec()),
    ]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

pub(crate) fn toggle_setting(ctx: &Arc<Ctx>, key: &str) -> Option<(String, Vec<Vec<Button>>)> {
    let (_, _, default) = TOGGLES.iter().find(|(name, _, _)| *name == key)?;
    let current = ctx.db.get_bool("heroku.main", key, *default);
    ctx.db.set("heroku.main", key, !current);
    Some(settings_card(ctx))
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuSettingsMod))
}
