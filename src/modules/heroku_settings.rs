// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Global userbot switches and nickname rules.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::Button;
use grammers_session::types::PeerId;
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
            "watcherbl" => self.watcher_blacklist(ctx, message, args).await,
            "nonickuser" => self.nonick_user(ctx, message).await,
            "nonickchat" => self.nonick_chat(ctx, message).await,
            "nonickcmd" => self.nonick_command(ctx, message, args).await,
            "nonickusers" => {
                let users = ctx.db.get_i64_list("heroku.main", "nonickusers");
                if users.is_empty() {
                    message.reply_html(ctx.tr("settings.nothing")).await?;
                    return Ok(());
                }
                let mut lines = Vec::new();
                for id in users {
                    let peer = PeerId::from_bot_api_dialog_id(id);
                    let name = match peer {
                        Some(peer_id) => match ctx.resolve_peer_ref(peer_id).await {
                            Some(peer_ref) => ctx
                                .client
                                .resolve_peer(peer_ref)
                                .await
                                .ok()
                                .and_then(|peer| peer.name().map(ToOwned::to_owned))
                                .unwrap_or_else(|| id.to_string()),
                            None => id.to_string(),
                        },
                        None => id.to_string(),
                    };
                    lines.push(format!(
                        "▫️ <b><a href=\"tg://user?id={id}\">{}</a></b>",
                        escape_html(&name)
                    ));
                }
                message
                    .reply_html(ctx.tf("settings.user_nn_list", &[&lines.join("\n")]))
                    .await?;
                Ok(())
            }
            "nonickchats" => {
                let chats = ctx.db.get_i64_list("heroku.main", "nonickchats");
                if chats.is_empty() {
                    message.reply_html(ctx.tr("settings.nothing")).await?;
                    return Ok(());
                }
                let lines = chats
                    .iter()
                    .map(|id| format!("▫️ <code>{id}</code>"))
                    .collect::<Vec<_>>()
                    .join("\n");
                message
                    .reply_html(ctx.tf("settings.chat_nn_list", &[&lines]))
                    .await?;
                Ok(())
            }
            "nonickcmds" => {
                let commands = ctx.db.get_string_list("heroku.main", "nonickcmds");
                if commands.is_empty() {
                    message.reply_html(ctx.tr("settings.nothing")).await?;
                    return Ok(());
                }
                let prefix = ctx
                    .prefixes(ctx.me_id)
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| ".".to_owned());
                let lines = commands
                    .iter()
                    .map(|command| {
                        format!(
                            "▫️ <code>{}{}</code>",
                            escape_html(&prefix),
                            escape_html(command)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                message
                    .reply_html(ctx.tf("settings.cmd_nn_list", &[&lines]))
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
        let disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        let mut lines = Vec::new();
        for module in ctx
            .modules
            .all()
            .iter()
            .filter(|module| module.has_watcher())
        {
            if !disabled.contains_key(module.name()) {
                lines.push(format!("♻️ {}", escape_html(module.name())));
            }
        }
        for (name, rules) in &disabled {
            lines.push(format!("💢 {} {}", escape_html(name), rules));
        }
        if lines.is_empty() {
            message.reply_html(ctx.tr("settings.nothing")).await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("settings.watchers_list", &[&lines.join("\n")]))
            .await?;
        Ok(())
    }

    async fn toggle_watcher(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("settings.watcher_args")).await?;
            return Ok(());
        };
        let Some(module) = ctx.modules.get(name) else {
            message
                .reply_html(ctx.tf("settings.watcher_404", &[&escape_html(name)]))
                .await?;
            return Ok(());
        };
        if !module.has_watcher() {
            message
                .reply_html(ctx.tf("settings.watcher_404", &[&escape_html(module.name())]))
                .await?;
            return Ok(());
        }

        let chats = args.flag("c");
        let mut pm = args.flag("p");
        let out = args.flag("o");
        let mut incoming = args.flag("i");
        if chats && pm {
            pm = false;
        }
        if out && incoming {
            incoming = false;
        }

        let mut disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        if chats || pm || out || incoming {
            let mut rules = Vec::new();
            if chats {
                rules.push(Value::from("only_chats"));
            }
            if pm {
                rules.push(Value::from("only_pm"));
            }
            if out {
                rules.push(Value::from("out"));
            }
            if incoming {
                rules.push(Value::from("in"));
            }
            disabled.insert(module.name().to_owned(), Value::Array(rules.clone()));
            ctx.db
                .set("heroku.main", "disabled_watchers", Value::Object(disabled));
            message
                .reply_html(format!(
                    "{} (<code>{}</code>)",
                    ctx.tf("settings.watcher_enabled", &[&escape_html(module.name())]),
                    Value::Array(rules)
                ))
                .await?;
            return Ok(());
        }

        let globally_disabled = disabled
            .get(module.name())
            .and_then(Value::as_array)
            .is_some_and(|rules| rules.iter().any(|rule| rule.as_str() == Some("*")));
        if globally_disabled {
            disabled.remove(module.name());
            ctx.db
                .set("heroku.main", "disabled_watchers", Value::Object(disabled));
            message
                .reply_html(ctx.tf("settings.watcher_enabled", &[&escape_html(module.name())]))
                .await?;
            return Ok(());
        }

        disabled.insert(
            module.name().to_owned(),
            Value::Array(vec![Value::from("*")]),
        );
        ctx.db
            .set("heroku.main", "disabled_watchers", Value::Object(disabled));
        message
            .reply_html(ctx.tf("settings.watcher_disabled", &[&escape_html(module.name())]))
            .await?;
        Ok(())
    }

    async fn watcher_blacklist(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("settings.watcher_args")).await?;
            return Ok(());
        };
        let Some(module) = ctx.modules.get(name) else {
            message
                .reply_html(ctx.tf("settings.watcher_404", &[&escape_html(name)]))
                .await?;
            return Ok(());
        };
        if !module.has_watcher() {
            message
                .reply_html(ctx.tf("settings.watcher_404", &[&escape_html(module.name())]))
                .await?;
            return Ok(());
        }
        let chat = message.chat_id();
        let mut disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        let mut rules = disabled
            .get(module.name())
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let present = rules.iter().any(|rule| {
            rule.as_i64() == Some(chat) || rule.as_str().is_some_and(|v| v == chat.to_string())
        });
        if present {
            rules.retain(|rule| {
                rule.as_i64() != Some(chat) && rule.as_str().is_none_or(|v| v != chat.to_string())
            });
            if rules.is_empty() {
                disabled.remove(module.name());
            } else {
                disabled.insert(module.name().to_owned(), Value::Array(rules));
            }
            ctx.db
                .set("heroku.main", "disabled_watchers", Value::Object(disabled));
            message
                .reply_html(format!(
                    "{} <b>in current chat</b>",
                    ctx.tf("settings.watcher_enabled", &[&escape_html(module.name())])
                ))
                .await?;
        } else {
            rules.push(Value::from(chat));
            disabled.insert(module.name().to_owned(), Value::Array(rules));
            ctx.db
                .set("heroku.main", "disabled_watchers", Value::Object(disabled));
            message
                .reply_html(format!(
                    "{} <b>in current chat</b>",
                    ctx.tf("settings.watcher_disabled", &[&escape_html(module.name())])
                ))
                .await?;
        }
        Ok(())
    }

    async fn nonick_user(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let Some(reply) = message.get_reply().await? else {
            message
                .reply_html(ctx.tr("settings.reply_required"))
                .await?;
            return Ok(());
        };
        let Some(peer_id) = reply.inner.sender_id() else {
            message
                .reply_html(ctx.tr("settings.reply_required"))
                .await?;
            return Ok(());
        };
        let Some(peer) = ctx.resolve_peer_ref(peer_id).await else {
            message
                .reply_html(ctx.tr("settings.reply_required"))
                .await?;
            return Ok(());
        };
        let user = super::common::peer_id(&peer);
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
            .reply_html(ctx.tf("settings.user_nn", &[if enabled { "on" } else { "off" }]))
            .await?;
        Ok(())
    }

    async fn nonick_chat(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        if message.is_private() {
            message
                .reply_html(ctx.tr("settings.private_not_allowed"))
                .await?;
            return Ok(());
        }
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
                "settings.cmd_nn",
                &[
                    &escape_html(&message.chat_display()),
                    if enabled { "on" } else { "off" },
                ],
            ))
            .await?;
        Ok(())
    }

    async fn nonick_command(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("settings.no_cmd")).await?;
            return Ok(());
        };
        let name = name.trim_start_matches('.');
        if ctx.modules.find_command(name).into_iter().next().is_none() {
            message.reply_html(ctx.tr("settings.cmd404")).await?;
            return Ok(());
        }
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
        let prefix = ctx
            .prefixes(ctx.me_id)
            .into_iter()
            .next()
            .unwrap_or_else(|| ".".to_owned());
        message
            .reply_html(ctx.tf(
                "settings.cmd_nn",
                &[
                    &escape_html(&format!("{prefix}{name}")),
                    if enabled { "on" } else { "off" },
                ],
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
        // Python reuses one button for `suggest_subscribe` and flips its
        // label depending on the current value.
        let label = if *key == "suggest_subscribe" {
            if enabled {
                ctx.tr("settings.suggest_subscribe")
            } else {
                ctx.tr("settings.do_not_suggest_subscribe")
            }
        } else {
            format!("{} {key}", if enabled { "✅" } else { "🚫" })
        };
        row.push(Button::data(
            label,
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
