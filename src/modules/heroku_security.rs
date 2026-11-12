// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Owners, security groups, temporary rules and permission masks.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{permission_bit, CommandDef, Module, ModuleFactory, PERMISSION_NAMES};
use crate::security::mask_to_names;
use crate::tg::{escape_html, Msg};

pub struct HerokuSecurityMod;

#[async_trait]
impl Module for HerokuSecurityMod {
    fn name(&self) -> &'static str {
        "HerokuSecurityMod"
    }

    fn description(&self) -> &'static str {
        "Manage owners, security groups and permissions"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("owneradd", "Add an owner").usage("-f <user>"),
            CommandDef::new("ownerrm", "Remove an owner").usage("<user>"),
            CommandDef::new("ownerlist", "List owners"),
            CommandDef::new("newsgroup", "Create a security group").usage("<name>"),
            CommandDef::new("sgroups", "List security groups"),
            CommandDef::new("sgroup", "Show a security group").usage("<name>"),
            CommandDef::new("delsgroup", "Delete a security group").usage("<name>"),
            CommandDef::new("sgroupadd", "Add a user to a security group").usage("<group> [user]"),
            CommandDef::new("sgroupdel", "Remove a user from a security group")
                .usage("<group> [user]"),
            CommandDef::new("sgrouprule", "Add a rule to a security group")
                .usage("<group> <command|module> <name>"),
            CommandDef::new("security", "Show permission masks").usage("[command]"),
            CommandDef::new("tsec", "Create a temporary security rule")
                .usage("<user|chat> <target> <rule> [duration]"),
            CommandDef::new("tsecrm", "Remove temporary security rules")
                .usage("<user|chat> <target>"),
            CommandDef::new("tsecclr", "Clear all temporary security rules"),
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
            "owneradd" => self.owner_add(ctx, message, args).await,
            "ownerrm" => self.owner_remove(ctx, message, args).await,
            "ownerlist" => self.owner_list(ctx, message).await,
            "newsgroup" => self.group_create(ctx, message, args).await,
            "sgroups" => self.group_list(ctx, message).await,
            "sgroup" => self.group_show(ctx, message, args).await,
            "delsgroup" => self.group_delete(ctx, message, args).await,
            "sgroupadd" => self.group_add_user(ctx, message, args, true).await,
            "sgroupdel" => self.group_add_user(ctx, message, args, false).await,
            "sgrouprule" => self.group_add_rule(ctx, message, args).await,
            "security" => self.masks(ctx, message, args).await,
            "tsec" => self.tsec(ctx, message, args).await,
            "tsecrm" => self.tsec_remove(ctx, message, args).await,
            "tsecclr" => {
                ctx.security.clear_tsec();
                message.reply_html(ctx.tr("security.tsec_cleared")).await?;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl HerokuSecurityMod {
    async fn resolve_user(ctx: &Arc<Ctx>, message: &Msg, raw: &str) -> Option<i64> {
        ctx.target_user(message, raw)
            .await
            .map(|peer| super::common::peer_id(&peer))
    }

    async fn owner_add(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let raw = args
            .list
            .iter()
            .filter(|item| !item.starts_with('-'))
            .cloned()
            .collect::<Vec<_>>()
            .join(" ");
        let Some(user) = Self::resolve_user(ctx, message, &raw).await else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        if !ctx.security.add_owner(user) {
            message.reply_html(ctx.tr("security.already")).await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.added", &[&user.to_string()]))
            .await?;
        Ok(())
    }

    async fn owner_remove(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(user) = Self::resolve_user(ctx, message, args.raw.as_str()).await else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        if user == ctx.me_id {
            message
                .reply_html(ctx.tr("security.cannot_remove_self"))
                .await?;
            return Ok(());
        }
        if !ctx.security.remove_owner(user, ctx.me_id) {
            message.reply_html(ctx.tr("security.not_present")).await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.removed", &[&user.to_string()]))
            .await?;
        Ok(())
    }

    async fn owner_list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let mut owners = ctx.security.owners();
        if !owners.contains(&ctx.me_id) {
            owners.push(ctx.me_id);
        }
        let lines = owners
            .iter()
            .map(|id| format!("▪️ <code>{id}</code>"))
            .collect::<Vec<_>>()
            .join("\n");
        message
            .reply_html(format!("{}\n{lines}", ctx.tr("security.owners")))
            .await?;
        Ok(())
    }

    async fn group_create(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        if !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
            message
                .reply_html("🚫 Only letters, digits and _ are allowed")
                .await?;
            return Ok(());
        }
        let mut groups = ctx.security.sgroups();
        if groups.contains_key(name) {
            message.reply_html(ctx.tr("security.already")).await?;
            return Ok(());
        }
        groups.insert(
            name.to_owned(),
            serde_json::json!({"users": [], "permissions": []}),
        );
        ctx.security.save_sgroups(groups);
        message
            .reply_html(ctx.tf("security.sgroup_created", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn group_list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let groups = ctx.security.sgroups();
        if groups.is_empty() {
            message
                .reply_html(ctx.tr("security.sgroup_empty").replace("{}", "*"))
                .await?;
            return Ok(());
        }
        let mut lines = vec![ctx.tr("security.sgroups")];
        for (name, group) in groups {
            let users = group
                .get("users")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            let rules = group
                .get("permissions")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            lines.push(format!(
                "▪️ <code>{}</code> — 👤 {} · 📜 {}",
                escape_html(&name),
                users,
                rules
            ));
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn group_show(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let groups = ctx.security.sgroups();
        let Some(group) = groups.get(name) else {
            message
                .reply_html(ctx.tf("security.sgroup_empty", &[&escape_html(name)]))
                .await?;
            return Ok(());
        };
        let mut lines = vec![ctx.tf("security.sgroup_created", &[&escape_html(name)])];
        if let Some(users) = group.get("users").and_then(Value::as_array) {
            for user in users {
                lines.push(format!(
                    "▪️ <a href=\"tg://user?id={}\">{}</a>",
                    user.as_i64().unwrap_or_default(),
                    user
                ));
            }
        }
        if let Some(rules) = group.get("permissions").and_then(Value::as_array) {
            for rule in rules {
                lines.push(format!(
                    "📜 <code>{}/{}</code>",
                    escape_html(rule.get("rule_type").and_then(Value::as_str).unwrap_or("?")),
                    escape_html(rule.get("rule").and_then(Value::as_str).unwrap_or("?"))
                ));
            }
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn group_delete(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        if !ctx.security.delete_sgroup(name) {
            message
                .reply_html(ctx.tf("security.sgroup_empty", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.sgroup_deleted", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn group_add_user(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        args: &Args,
        add: bool,
    ) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let raw = args.list[1..].join(" ");
        let Some(user) = Self::resolve_user(ctx, message, &raw).await else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        let ok = if add {
            ctx.security.add_sgroup(name, user)
        } else {
            ctx.security.remove_sgroup_user(name, user)
        };
        if !ok {
            message
                .reply_html(ctx.tf("security.sgroup_empty", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.rule_added", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn group_add_rule(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let (Some(name), Some(rule_type), Some(rule)) = (args.get(0), args.get(1), args.get(2))
        else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let rule_type = match rule_type {
            "command" | "module" => rule_type,
            other => {
                message
                    .reply_html(format!(
                        "🚫 Unknown rule type <code>{}</code>",
                        escape_html(other)
                    ))
                    .await?;
                return Ok(());
            }
        };
        if !ctx.security.add_group_rule(name, rule_type, rule) {
            message
                .reply_html(ctx.tf("security.sgroup_empty", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.rule_added", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn masks(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if args.get(0) == Some("set") {
            let (Some(module), Some(command), Some(mask)) = (args.get(1), args.get(2), args.get(3))
            else {
                message.reply_html(ctx.tr("common.no_args")).await?;
                return Ok(());
            };
            let Some(mask) = parse_mask(mask) else {
                message.reply_html(ctx.tr("config.invalid")).await?;
                return Ok(());
            };
            let Some(module) = ctx.modules.get(module) else {
                message
                    .reply_html(ctx.tr("settings.module_not_found"))
                    .await?;
                return Ok(());
            };
            ctx.security.set_command_mask(module.name(), command, mask);
            message
                .reply_html(ctx.tf("config.set", &[&escape_html(command), &mask.to_string()]))
                .await?;
            return Ok(());
        }

        if let Some(command) = args.get(0) {
            let Some((module, def)) = ctx.modules.find_command(command).into_iter().next() else {
                message
                    .reply_html(ctx.tr("settings.command_not_found"))
                    .await?;
                return Ok(());
            };
            let mask = ctx
                .security
                .command_mask(module.name(), def.name, def.security);
            message
                .reply_html(format!(
                    "🛡 <b>{}.{}</b>\n▫️ mask: <code>{}</code>\n▫️ {}\n\n<i>.security set {} {} &lt;mask&gt;</i>",
                    escape_html(module.name()),
                    escape_html(def.name),
                    mask,
                    mask_to_names(mask).join(", "),
                    module.name(),
                    def.name
                ))
                .await?;
            return Ok(());
        }

        let mut lines = vec![ctx.tr("security.masks")];
        lines.push(format!(
            "▫️ default: <code>{}</code> ({})",
            ctx.security.default_mask(),
            mask_to_names(ctx.security.default_mask()).join(", ")
        ));
        lines.push(format!(
            "▫️ bounding: <code>{}</code> ({})",
            ctx.security.bounding_mask(),
            mask_to_names(ctx.security.bounding_mask()).join(", ")
        ));
        lines.push(format!(
            "▫️ owners: <code>{}</code>",
            ctx.security.owners().len()
        ));
        lines.push(String::new());
        lines.push("<i>Permission names:</i>".to_owned());
        let names = PERMISSION_NAMES
            .iter()
            .map(|(name, bit)| format!("<code>{name}</code>=<code>{bit}</code>"))
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(names);
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn tsec(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let (Some(kind), Some(target_arg), Some(rule_type), Some(rule)) =
            (args.get(0), args.get(1), args.get(2), args.get(3))
        else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        let duration = args.get(4).map(parse_duration).unwrap_or(0);

        let target = match kind {
            "user" => match Self::resolve_user(ctx, message, target_arg).await {
                Some(id) => id,
                None => {
                    message.reply_html(ctx.tr("common.who")).await?;
                    return Ok(());
                }
            },
            "chat" => match ctx.resolve(target_arg).await {
                Some(peer) => super::common::peer_id(&peer),
                None => {
                    message.reply_html(ctx.tr("common.not_found")).await?;
                    return Ok(());
                }
            },
            "sgroup" => {
                if ctx.security.sgroups().contains_key(target_arg) {
                    if !ctx.security.add_group_rule(target_arg, rule_type, rule) {
                        message.reply_html(ctx.tr("common.not_found")).await?;
                        return Ok(());
                    }
                    message
                        .reply_html(ctx.tf("security.rule_added", &[&escape_html(target_arg)]))
                        .await?;
                    return Ok(());
                }
                message.reply_html(ctx.tr("common.not_found")).await?;
                return Ok(());
            }
            _ => {
                message.reply_html(ctx.tr("common.no_args")).await?;
                return Ok(());
            }
        };

        if !matches!(rule_type, "command" | "module") {
            message
                .reply_html("🚫 Rule type must be <code>command</code> or <code>module</code>")
                .await?;
            return Ok(());
        }

        ctx.security.add_tsec(
            if kind == "chat" { "chat" } else { "user" },
            target,
            rule_type,
            rule,
            duration,
            target_arg,
        );
        message
            .reply_html(format!(
                "✅ <code>{}</code> <code>{}</code> → <code>{}/{}</code>",
                escape_html(kind),
                target,
                escape_html(rule_type),
                escape_html(rule)
            ))
            .await?;
        Ok(())
    }

    async fn tsec_remove(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let (Some(kind), Some(target)) = (args.get(0), args.get(1)) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };
        if !matches!(kind, "user" | "chat") {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        }
        let target = if let Ok(id) = target.parse::<i64>() {
            id
        } else if let Some(peer) = ctx.resolve(target).await {
            super::common::peer_id(&peer)
        } else {
            message.reply_html(ctx.tr("common.who")).await?;
            return Ok(());
        };
        if !ctx.security.remove_tsec(kind, target) {
            message.reply_html(ctx.tr("security.tsec_empty")).await?;
            return Ok(());
        }
        message.reply_html(ctx.tr("security.tsec_removed")).await?;
        Ok(())
    }
}

fn parse_mask(raw: &str) -> Option<u32> {
    if let Ok(mask) = raw.parse::<u32>() {
        return Some(mask);
    }
    let mut mask = 0;
    for name in raw.split([',', '+', '|']) {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        mask |= permission_bit(name)?;
    }
    Some(mask)
}

fn parse_duration(raw: &str) -> i64 {
    let raw = raw.trim().to_lowercase();
    let (number, multiplier) = match raw.chars().last() {
        Some('s') => (&raw[..raw.len() - 1], 1),
        Some('m') => (&raw[..raw.len() - 1], 60),
        Some('h') => (&raw[..raw.len() - 1], 3_600),
        Some('d') => (&raw[..raw.len() - 1], 86_400),
        _ => (raw.as_str(), 1),
    };
    number.parse::<i64>().unwrap_or(0) * multiplier
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuSecurityMod))
}
