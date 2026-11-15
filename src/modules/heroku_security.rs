// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Owners, security groups, temporary rules and permission masks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::Button;
use grammers_session::types::{PeerId, PeerRef};
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
                .usage("<user|chat|sgroup> [target] <rule> [duration]"),
            CommandDef::new("tsecrm", "Remove temporary security rules")
                .usage("<user|chat|sgroup> [target] <rule>"),
            CommandDef::new("tsecclr", "Clear all rules of a user, chat or group")
                .usage("<user|chat|sgroup> [target]"),
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
            "security" => {
                if args.list.is_empty() {
                    let (html, buttons) = security_card(ctx);
                    if ctx
                        .inline
                        .send_to_user(ctx, ctx.me_id, html, buttons)
                        .await
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                self.masks(ctx, message, args).await
            }
            "tsec" => self.tsec(ctx, message, args).await,
            "tsecrm" => self.tsec_remove(ctx, message, args).await,
            "tsecclr" => self.tsec_clear(ctx, message, args).await,
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
        let mut force = false;
        let mut nonick = false;
        let mut prefix: Vec<String> = Vec::new();
        let mut targets: Vec<String> = Vec::new();
        let tokens = crate::args::split_quotes(&args.raw);
        let mut iter = tokens.iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-f" | "--force" => force = true,
                "-n" | "--nonick" => nonick = true,
                "-p" | "--prefix" => match iter.next() {
                    Some(value) if !value.starts_with('-') => prefix.push(value.clone()),
                    _ => {
                        message
                            .reply_html(ctx.tr("security.owneradd_usage"))
                            .await?;
                        return Ok(());
                    }
                },
                value => targets.push(value.to_owned()),
            }
        }

        if targets.len() > 1 {
            message
                .reply_html(ctx.tr("security.owneradd_usage"))
                .await?;
            return Ok(());
        }

        for value in &prefix {
            if !valid_prefix(ctx, value) {
                message
                    .reply_html(ctx.tr("security.owner_prefix_invalid"))
                    .await?;
                return Ok(());
            }
        }

        let raw = targets.join(" ");
        let Some(user) = Self::resolve_user(ctx, message, &raw).await else {
            message.reply_html(ctx.tr("security.no_user")).await?;
            return Ok(());
        };

        if user == ctx.me_id {
            message.reply_html(ctx.tr("security.self")).await?;
            return Ok(());
        }

        if ctx.security.is_owner(user) {
            let name = user_display_name(ctx, user).await;
            message
                .reply_html(ctx.tf(
                    "security.owner_exists",
                    &[&user.to_string(), &user_link(user, &name)],
                ))
                .await?;
            return Ok(());
        }

        let chat = message.sender_id().unwrap_or(ctx.me_id);
        if !force {
            let (html, buttons) = owner_confirm_card(ctx, user, nonick, prefix.clone()).await;
            if ctx
                .inline
                .send_to_user(ctx, chat, html, buttons)
                .await
                .is_ok()
            {
                return Ok(());
            }
        }

        let (html, buttons) = apply_owner_add(ctx, user, nonick, prefix).await;
        if ctx
            .inline
            .send_to_user(ctx, chat, html.clone(), buttons)
            .await
            .is_err()
        {
            message.reply_html(html).await?;
        }
        Ok(())
    }

    async fn owner_remove(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(user) = Self::resolve_user(ctx, message, args.raw.as_str()).await else {
            message.reply_html(ctx.tr("security.no_user")).await?;
            return Ok(());
        };
        if user == ctx.me_id {
            message.reply_html(ctx.tr("security.self")).await?;
            return Ok(());
        }
        let name = user_display_name(ctx, user).await;
        ctx.security.remove_owner(user, ctx.me_id);
        message
            .reply_html(ctx.tf(
                "security.owner_removed",
                &[&user.to_string(), &user_link(user, &name)],
            ))
            .await?;
        Ok(())
    }

    async fn owner_list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let owners = visible_owners(ctx);
        if owners.is_empty() {
            message.reply_html(ctx.tr("security.no_owner")).await?;
            return Ok(());
        }
        let mut lines = Vec::new();
        for id in owners {
            let name = user_display_name(ctx, id).await;
            let prefixes = format_prefixes(&ctx.prefixes(id));
            let prefix = if prefixes.is_empty() {
                String::new()
            } else {
                format!(" (<code>{}</code>)", escape_html(&prefixes))
            };
            lines.push(format!(
                "{}{prefix}",
                ctx.tf("security.li", &[&format!("tg://user?id={id}"), &name])
            ));
        }
        message
            .reply_html(ctx.tf("security.owner_list", &[&lines.join("\n")]))
            .await?;
        Ok(())
    }

    async fn group_create(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };
        if !name.chars().all(|ch| ch.is_alphanumeric()) {
            message.reply_html(ctx.tr("security.invalid_name")).await?;
            return Ok(());
        }
        let mut groups = ctx.security.sgroups();
        if groups.contains_key(name) {
            message
                .reply_html(ctx.tf("security.sgroup_already_exists", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        groups.insert(
            name.to_owned(),
            serde_json::json!({"users": [], "permissions": []}),
        );
        ctx.security.save_sgroups(groups);
        message
            .reply_html(ctx.tf("security.created_sgroup", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn group_list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let groups = ctx.security.sgroups();
        let mut entries = Vec::new();
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
            entries.push(ctx.tf(
                "security.sgroup_li",
                &[&escape_html(&name), &users.to_string(), &rules.to_string()],
            ));
        }
        message
            .reply_html(ctx.tf("security.sgroups_list", &[&entries.join("\n")]))
            .await?;
        Ok(())
    }

    async fn group_show(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };
        let groups = ctx.security.sgroups();
        let Some(group) = groups.get(name) else {
            message
                .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                .await?;
            return Ok(());
        };

        let users = group
            .get("users")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let users_block = if users.is_empty() {
            ctx.tr("security.no_users")
        } else {
            let mut lines = Vec::new();
            for user in users {
                let id = user.as_i64().unwrap_or_default();
                let display = user_display_name(ctx, id).await;
                lines.push(ctx.tf("security.li", &[&format!("tg://user?id={id}"), &display]));
            }
            ctx.tf("security.users_list", &[&lines.join("\n")])
        };

        let rules = group
            .get("permissions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let rules_block = if rules.is_empty() {
            ctx.tr("security.no_permissions")
        } else {
            let mut lines = Vec::new();
            for rule in rules {
                let rule_type = rule
                    .get("rule_type")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                let rule_name = rule.get("rule").and_then(Value::as_str).unwrap_or("?");
                let expires = rule.get("expires").and_then(Value::as_i64).unwrap_or(0);
                let until = if expires == 0 {
                    ctx.tr("security.forever")
                } else {
                    format!("{} {}", ctx.tr("security.until"), format_timestamp(expires))
                };
                lines.push(format!(
                    "▫️ <b>{}</b> <code>{}</code> <b>{until}</b>",
                    escape_html(&ctx.tr(&format!("security.{rule_type}"))),
                    escape_html(rule_name)
                ));
            }
            ctx.tf("security.permissions_list", &[&lines.join("\n")])
        };

        message
            .reply_html(ctx.tf(
                "security.sgroup_info",
                &[&escape_html(name), &users_block, &rules_block],
            ))
            .await?;
        Ok(())
    }

    async fn group_delete(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };
        if !ctx.security.delete_sgroup(name) {
            message
                .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.deleted_sgroup", &[&escape_html(name)]))
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
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };
        let explicit = args.list.get(1).cloned();
        let raw = if let Some(value) = explicit {
            value
        } else if message.get_reply().await?.is_some() {
            String::new()
        } else {
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };

        let Some(user) = Self::resolve_user(ctx, message, &raw).await else {
            message.reply_html(ctx.tr("security.no_user")).await?;
            return Ok(());
        };
        if user == ctx.me_id {
            message.reply_html(ctx.tr("security.self")).await?;
            return Ok(());
        }

        let groups = ctx.security.sgroups();
        let Some(group) = groups.get(name) else {
            message
                .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                .await?;
            return Ok(());
        };
        let present = group
            .get("users")
            .and_then(Value::as_array)
            .is_some_and(|users| users.iter().any(|id| id.as_i64() == Some(user)));
        let display = user_display_name(ctx, user).await;

        if add {
            if present {
                message
                    .reply_html(ctx.tf(
                        "security.user_already_in_sgroup",
                        &[&display, &escape_html(name)],
                    ))
                    .await?;
                return Ok(());
            }
            ctx.security.add_sgroup(name, user);
            message
                .reply_html(ctx.tf(
                    "security.user_added_to_sgroup",
                    &[&display, &escape_html(name)],
                ))
                .await?;
        } else {
            if !present {
                message
                    .reply_html(ctx.tf(
                        "security.user_not_in_sgroup",
                        &[&display, &escape_html(name)],
                    ))
                    .await?;
                return Ok(());
            }
            ctx.security.remove_sgroup_user(name, user);
            message
                .reply_html(ctx.tf(
                    "security.user_removed_from_sgroup",
                    &[&display, &escape_html(name)],
                ))
                .await?;
        }
        Ok(())
    }

    async fn group_add_rule(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let (Some(name), Some(rule_type), Some(rule)) = (args.get(0), args.get(1), args.get(2))
        else {
            message.reply_html(ctx.tr("security.no_args")).await?;
            return Ok(());
        };
        if !matches!(rule_type, "command" | "module") {
            message.reply_html(ctx.tr("security.no_rule")).await?;
            return Ok(());
        }
        if !ctx.security.add_group_rule(name, rule_type, rule) {
            message
                .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                .await?;
            return Ok(());
        }
        message
            .reply_html(ctx.tf("security.rule_added_sgroup", &[&escape_html(name)]))
            .await?;
        Ok(())
    }

    async fn masks(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        if args.get(0) == Some("set") {
            let (Some(module), Some(command), Some(mask)) = (args.get(1), args.get(2), args.get(3))
            else {
                message.reply_html(ctx.tr("security.no_args")).await?;
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
                    .reply_html(ctx.tf("security.no_command", &[&escape_html(command)]))
                    .await?;
                return Ok(());
            };
            let user_id = message.sender_id().unwrap_or(ctx.me_id);
            if let Some((html, buttons)) = command_mask_card(ctx, module.name(), def.name) {
                if ctx
                    .inline
                    .send_to_user(ctx, user_id, html, buttons)
                    .await
                    .is_ok()
                {
                    return Ok(());
                }
            }
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

        let (html, buttons) = security_card(ctx);
        let user_id = message.sender_id().unwrap_or(ctx.me_id);
        if ctx
            .inline
            .send_to_user(ctx, user_id, html, buttons)
            .await
            .is_ok()
        {
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
        let list = args.list.clone();
        if list.is_empty() {
            let rules = describe_rules(ctx).await;
            if rules.is_empty() {
                message.reply_html(ctx.tr("security.no_rules")).await?;
            } else {
                message
                    .reply_html(ctx.tf("security.rules", &[&rules]))
                    .await?;
            }
            return Ok(());
        }

        let kind = list[0].as_str();
        if !matches!(kind, "user" | "chat" | "sgroup") {
            message.reply_html(ctx.tr("security.what")).await?;
            return Ok(());
        }

        let target = match kind {
            "sgroup" => {
                let Some(name) = list.get(1) else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                if !ctx.security.sgroups().contains_key(name) {
                    message
                        .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                        .await?;
                    return Ok(());
                }
                RuleTarget {
                    kind: "sgroup".to_owned(),
                    id: 0,
                    name: name.clone(),
                    url: String::new(),
                }
            }
            "chat" => {
                let peer = if let Some(value) = list.get(1) {
                    if value.chars().all(|ch| ch.is_ascii_digit() || ch == '-')
                        || value.starts_with('@')
                    {
                        ctx.resolve(value).await
                    } else {
                        message.peer_ref().await.ok()
                    }
                } else if message.is_private() {
                    None
                } else {
                    message.peer_ref().await.ok()
                };
                let Some(peer) = peer else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                let name = chat_name(ctx, &peer).await;
                RuleTarget {
                    kind: "chat".to_owned(),
                    id,
                    name,
                    url: format!("tg://user?id={id}"),
                }
            }
            _ => {
                let peer = if let Some(value) = list.get(1) {
                    if value.chars().all(|ch| ch.is_ascii_digit() || ch == '-')
                        || value.starts_with('@')
                    {
                        ctx.resolve(value).await
                    } else {
                        None
                    }
                } else if message.is_private() {
                    message.peer_ref().await.ok()
                } else {
                    None
                };
                let peer = match peer {
                    Some(peer) => Some(peer),
                    None => {
                        if let Some(reply) = message.get_reply().await? {
                            match reply.inner.sender_id() {
                                Some(id) => ctx.resolve_peer_ref(id).await,
                                None => None,
                            }
                        } else {
                            None
                        }
                    }
                };
                let Some(peer) = peer else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                if ctx.security.is_owner(id) {
                    message.reply_html(ctx.tr("security.owner_target")).await?;
                    return Ok(());
                }
                let name = user_display_name(ctx, id).await;
                RuleTarget {
                    kind: "user".to_owned(),
                    id,
                    name,
                    url: format!("tg://user?id={id}"),
                }
            }
        };

        let mut candidates: Vec<String> = Vec::new();
        for token in list.iter().skip(1) {
            for rule in lookup_rules(ctx, token) {
                if !candidates.contains(&rule) {
                    candidates.push(rule);
                }
            }
        }
        if candidates.is_empty() {
            message.reply_html(ctx.tr("security.no_rule")).await?;
            return Ok(());
        }

        let duration = extract_time(&list);
        let chat = message.sender_id().unwrap_or(ctx.me_id);

        if candidates.len() > 1 {
            let (html, buttons) = rule_choice_card(ctx, &target, &candidates, duration);
            if ctx
                .inline
                .send_to_user(ctx, chat, html.clone(), buttons)
                .await
                .is_ok()
            {
                return Ok(());
            }
            let mut lines = Vec::new();
            for rule in &candidates {
                let (rule_type, rule_name) = split_rule(rule);
                lines.push(format!(
                    "🛡 <b>{}</b> <code>{}</code>",
                    escape_html(&ctx.tr(&format!("security.{rule_type}"))),
                    escape_html(rule_name)
                ));
            }
            message
                .reply_html(ctx.tf("security.multiple_rules", &[&lines.join("\n")]))
                .await?;
            return Ok(());
        }

        let pending = PendingRule {
            target,
            rule: candidates.remove(0),
            duration,
        };
        let (html, buttons) = rule_confirm_card(ctx, pending).await;
        if ctx
            .inline
            .send_to_user(ctx, chat, html.clone(), buttons)
            .await
            .is_err()
        {
            message.reply_html(html).await?;
        }
        Ok(())
    }

    async fn tsec_remove(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let list = args.list.clone();
        let Some(kind) = list.first().map(String::as_str) else {
            message.reply_html(ctx.tr("security.no_target")).await?;
            return Ok(());
        };
        let Some(rule) = list.last().filter(|_| list.len() >= 2).cloned() else {
            message.reply_html(ctx.tr("security.no_target")).await?;
            return Ok(());
        };

        match kind {
            "user" => {
                let peer = if message.is_private() {
                    message.peer_ref().await.ok()
                } else if let Some(reply) = message.get_reply().await? {
                    match reply.inner.sender_id() {
                        Some(id) => ctx.resolve_peer_ref(id).await,
                        None => None,
                    }
                } else if list.len() >= 3 {
                    ctx.resolve(&list[1]).await
                } else {
                    None
                };
                let Some(peer) = peer else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                if !ctx.security.remove_tsec_rule("user", id, &rule) {
                    message.reply_html(ctx.tr("security.no_rules")).await?;
                    return Ok(());
                }
                let name = user_display_name(ctx, id).await;
                message
                    .reply_html(ctx.tf(
                        "security.rule_removed",
                        &[
                            &format!("tg://user?id={id}"),
                            &user_link(id, &name),
                            &escape_html(&rule),
                        ],
                    ))
                    .await?;
            }
            "sgroup" => {
                let (Some(name), true) = (list.get(1), list.len() >= 3) else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                if !ctx.security.sgroups().contains_key(name) {
                    message
                        .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                        .await?;
                    return Ok(());
                }
                if !ctx.security.remove_group_rule(name, &rule) {
                    message.reply_html(ctx.tr("security.no_rules")).await?;
                    return Ok(());
                }
                message
                    .reply_html(ctx.tf(
                        "security.rule_removed",
                        &["", &escape_html(name), &escape_html(&rule)],
                    ))
                    .await?;
            }
            "chat" => {
                if message.is_private() {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                }
                let Some(peer) = message.peer_ref().await.ok() else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                if !ctx.security.remove_tsec_rule("chat", id, &rule) {
                    message.reply_html(ctx.tr("security.no_rules")).await?;
                    return Ok(());
                }
                let name = chat_name(ctx, &peer).await;
                message
                    .reply_html(ctx.tf(
                        "security.rule_removed",
                        &[
                            &format!("tg://user?id={id}"),
                            &user_link(id, &name),
                            &escape_html(&rule),
                        ],
                    ))
                    .await?;
            }
            _ => {
                message.reply_html(ctx.tr("security.no_target")).await?;
            }
        }
        Ok(())
    }

    async fn tsec_clear(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let list = args.list.clone();
        let Some(kind) = list.first().map(String::as_str) else {
            message.reply_html(ctx.tr("security.no_target")).await?;
            return Ok(());
        };
        match kind {
            "user" => {
                let peer = if message.is_private() {
                    message.peer_ref().await.ok()
                } else if let Some(reply) = message.get_reply().await? {
                    match reply.inner.sender_id() {
                        Some(id) => ctx.resolve_peer_ref(id).await,
                        None => None,
                    }
                } else if list.len() >= 2 {
                    ctx.resolve(&list[1]).await
                } else {
                    None
                };
                let Some(peer) = peer else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                if !ctx.security.clear_tsec_target("user", id) {
                    message.reply_html(ctx.tr("security.no_rules")).await?;
                    return Ok(());
                }
                let name = user_display_name(ctx, id).await;
                message
                    .reply_html(ctx.tf(
                        "security.rules_removed",
                        &[&format!("tg://user?id={id}"), &user_link(id, &name)],
                    ))
                    .await?;
            }
            "sgroup" => {
                let Some(name) = list.get(1) else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                if !ctx.security.sgroups().contains_key(name) {
                    message
                        .reply_html(ctx.tf("security.sgroup_not_found", &[&escape_html(name)]))
                        .await?;
                    return Ok(());
                }
                ctx.security.clear_group_rules(name);
                message
                    .reply_html(ctx.tf("security.rules_removed", &["", &escape_html(name)]))
                    .await?;
            }
            "chat" => {
                if message.is_private() {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                }
                let Some(peer) = message.peer_ref().await.ok() else {
                    message.reply_html(ctx.tr("security.no_target")).await?;
                    return Ok(());
                };
                let id = super::common::peer_id(&peer);
                if !ctx.security.clear_tsec_target("chat", id) {
                    message.reply_html(ctx.tr("security.no_rules")).await?;
                    return Ok(());
                }
                let name = chat_name(ctx, &peer).await;
                message
                    .reply_html(ctx.tf(
                        "security.rules_removed",
                        &[&format!("tg://user?id={id}"), &user_link(id, &name)],
                    ))
                    .await?;
            }
            _ => {
                message.reply_html(ctx.tr("security.no_target")).await?;
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct RuleTarget {
    pub kind: String,
    pub id: i64,
    pub name: String,
    pub url: String,
}

#[derive(Clone)]
pub(crate) struct PendingRule {
    pub target: RuleTarget,
    pub rule: String,
    pub duration: i64,
}

static PENDING_RULES: LazyLock<Mutex<HashMap<u32, PendingRule>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static RULE_SEQ: AtomicU32 = AtomicU32::new(1);

fn stash_rule(rule: PendingRule) -> u32 {
    let id = RULE_SEQ.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut pending) = PENDING_RULES.lock() {
        // Keep the map from growing if a user keeps opening confirmation cards.
        if pending.len() > 256 {
            pending.clear();
        }
        pending.insert(id, rule);
    }
    id
}

fn take_rule(id: u32) -> Option<PendingRule> {
    PENDING_RULES.lock().ok()?.remove(&id)
}

fn hex_encode(value: &str) -> String {
    value.bytes().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn hex_decode(value: &str) -> Option<String> {
    if value.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
}

fn user_link(id: i64, name: &str) -> String {
    format!("<a href=\"tg://user?id={id}\">{name}</a>")
}

async fn user_display_name(ctx: &Arc<Ctx>, user_id: i64) -> String {
    if user_id == ctx.me_id {
        return escape_html(&ctx.me_name);
    }
    if let Some(peer_id) = PeerId::from_bot_api_dialog_id(user_id) {
        if let Some(peer_ref) = ctx.resolve_peer_ref(peer_id).await {
            if let Ok(peer) = ctx.client.resolve_peer(peer_ref).await {
                if let Some(name) = peer.name() {
                    return escape_html(name);
                }
            }
        }
    }
    user_id.to_string()
}

async fn chat_name(ctx: &Arc<Ctx>, peer_ref: &PeerRef) -> String {
    match ctx.client.resolve_peer(*peer_ref).await {
        Ok(peer) => escape_html(peer.name().unwrap_or("chat")),
        Err(_) => super::common::peer_id(peer_ref).to_string(),
    }
}

fn format_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn format_prefixes(prefixes: &[String]) -> String {
    match prefixes {
        [] => ".".to_owned(),
        [single] => single.clone(),
        many => format!("[{}]", many.join(", ")),
    }
}

fn valid_prefix(ctx: &Arc<Ctx>, prefix: &str) -> bool {
    let allow_long = ctx
        .db
        .get_bool("CoreMod", "allow_nonstandart_prefixes", false);
    prefix
        .chars()
        .all(|ch| ch == 's' || (!ch.is_whitespace() && (allow_long || ch.len_utf8() == 1)))
}

fn add_nonick(ctx: &Arc<Ctx>, user: i64) {
    let mut users = ctx.db.get_i64_list("heroku.main", "nonickusers");
    if !users.contains(&user) {
        users.push(user);
        ctx.db
            .set("heroku.main", "nonickusers", serde_json::json!(users));
    }
}

pub(crate) async fn owner_confirm_card(
    ctx: &Arc<Ctx>,
    user: i64,
    nonick: bool,
    prefix: Vec<String>,
) -> (String, Vec<Vec<Button>>) {
    let name = user_display_name(ctx, user).await;
    let mut html = ctx.tf(
        "security.warning",
        &[&user.to_string(), &user_link(user, &name), "owner"],
    );
    if !prefix.is_empty() {
        html.push_str("\n\n");
        html.push_str(&ctx.tf(
            "security.owner_prefix",
            &[&escape_html(&format_prefixes(&prefix))],
        ));
    }
    let data = format!(
        "heroku:sec:o:cf:{user}:{}:{}",
        u8::from(nonick),
        hex_encode(&prefix.join("\n"))
    );
    (
        html,
        vec![
            vec![Button::data(
                ctx.tr("security.cancel_btn"),
                b"heroku:close".to_vec(),
            )],
            vec![Button::data(
                ctx.tr("security.confirm_btn"),
                data.into_bytes(),
            )],
        ],
    )
}

pub(crate) async fn apply_owner_add(
    ctx: &Arc<Ctx>,
    user: i64,
    nonick: bool,
    prefix: Vec<String>,
) -> (String, Vec<Vec<Button>>) {
    ctx.security.add_owner(user);
    let name = user_display_name(ctx, user).await;
    let mut html = ctx.tf(
        "security.owner_added",
        &[&user.to_string(), &user_link(user, &name)],
    );

    if !prefix.is_empty() {
        let mut personal = ctx.db.get_map("heroku.main", "command_prefixes");
        let value = if prefix.len() == 1 {
            Value::String(prefix[0].clone())
        } else {
            serde_json::json!(prefix)
        };
        personal.insert(user.to_string(), value);
        ctx.db
            .set("heroku.main", "command_prefixes", Value::Object(personal));
        html.push_str("\n\n");
        html.push_str(&ctx.tf(
            "security.owner_prefix",
            &[&escape_html(&format_prefixes(&prefix))],
        ));
    }

    if nonick {
        add_nonick(ctx, user);
        html.push_str("\n\n");
        html.push_str(&ctx.tf(
            "security.user_nn",
            &[&user.to_string(), &user_link(user, &name)],
        ));
        (
            html,
            vec![vec![Button::data("✖️ Close", b"heroku:close".to_vec())]],
        )
    } else {
        html.push_str("\n\n");
        html.push_str(&ctx.tr("security.suggest_nonick"));
        (
            html,
            vec![
                vec![Button::data(
                    ctx.tr("security.cancel_btn"),
                    b"heroku:close".to_vec(),
                )],
                vec![Button::data(
                    ctx.tr("security.enable_nonick_btn"),
                    format!("heroku:sec:nn:{user}").into_bytes(),
                )],
            ],
        )
    }
}

pub(crate) async fn enable_nonick(ctx: &Arc<Ctx>, user: i64) -> String {
    add_nonick(ctx, user);
    let name = user_display_name(ctx, user).await;
    ctx.tf(
        "security.user_nn",
        &[&user.to_string(), &user_link(user, &name)],
    )
}

fn split_rule(rule: &str) -> (&str, &str) {
    rule.split_once('/').unwrap_or(("command", rule))
}

fn human_duration(ctx: &Arc<Ctx>, seconds: i64) -> String {
    if seconds <= 0 {
        return ctx.tr("security.forever");
    }
    let (value, one, many) = if seconds >= 86_400 {
        (seconds / 86_400, "day", "days")
    } else if seconds >= 3_600 {
        (seconds / 3_600, "hour", "hours")
    } else if seconds >= 60 {
        (seconds / 60, "minute", "minutes")
    } else {
        (seconds, "second", "seconds")
    };
    let key = if value > 1 { many } else { one };
    ctx.tf(&format!("security.{key}"), &[&value.to_string()])
}

fn time_label(ctx: &Arc<Ctx>, duration: i64) -> String {
    if duration <= 0 {
        ctx.tr("security.forever")
    } else {
        format!(
            "{} {}",
            ctx.tr("security.for"),
            human_duration(ctx, duration)
        )
    }
}

pub(crate) fn rule_choice_card(
    ctx: &Arc<Ctx>,
    target: &RuleTarget,
    candidates: &[String],
    duration: i64,
) -> (String, Vec<Vec<Button>>) {
    let mut lines = Vec::new();
    for rule in candidates {
        let (rule_type, rule_name) = split_rule(rule);
        lines.push(format!(
            "🛡 <b>{}</b> <code>{}</code>",
            escape_html(&ctx.tr(&format!("security.{rule_type}"))),
            escape_html(rule_name)
        ));
    }
    let html = ctx.tf("security.multiple_rules", &[&lines.join("\n")]);

    let mut rows = Vec::new();
    for rule in candidates {
        let (rule_type, rule_name) = split_rule(rule);
        let id = stash_rule(PendingRule {
            target: target.clone(),
            rule: rule.clone(),
            duration,
        });
        rows.push(vec![Button::data(
            format!(
                "🛡 {} {}",
                ctx.tr(&format!("security.{rule_type}")),
                rule_name
            ),
            format!("heroku:sec:tc:{id}").into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("security.cancel_btn"),
        b"heroku:close".to_vec(),
    )]);
    (html, rows)
}

pub(crate) async fn rule_confirm_card(
    ctx: &Arc<Ctx>,
    pending: PendingRule,
) -> (String, Vec<Vec<Button>>) {
    let (rule_type, rule_name) = split_rule(&pending.rule);
    let html = ctx.tf(
        "security.confirm_rule",
        &[
            &ctx.tr(&format!("security.{}", pending.target.kind)),
            &pending.target.url,
            &escape_html(&pending.target.name),
            &ctx.tr(&format!("security.{rule_type}")),
            &escape_html(rule_name),
            &time_label(ctx, pending.duration),
        ],
    );
    let id = stash_rule(pending);
    (
        html,
        vec![
            vec![Button::data(
                ctx.tr("security.confirm_btn"),
                format!("heroku:sec:tr:{id}").into_bytes(),
            )],
            vec![Button::data(
                ctx.tr("security.cancel_btn"),
                b"heroku:close".to_vec(),
            )],
        ],
    )
}

/// Turn a stored choice into the confirmation card (second step of `.tsec`).
pub(crate) async fn choose_rule(ctx: &Arc<Ctx>, id: u32) -> Option<(String, Vec<Vec<Button>>)> {
    let pending = PENDING_RULES.lock().ok()?.get(&id).cloned()?;
    Some(rule_confirm_card(ctx, pending).await)
}

pub(crate) fn apply_rule(ctx: &Arc<Ctx>, id: u32) -> String {
    let Some(pending) = take_rule(id) else {
        return ctx.tr("security.no_rule");
    };
    let (rule_type, rule_name) = split_rule(&pending.rule);
    if pending.target.kind == "sgroup" {
        ctx.security
            .add_group_rule(&pending.target.name, rule_type, rule_name);
    } else {
        ctx.security.add_tsec(
            &pending.target.kind,
            pending.target.id,
            rule_type,
            rule_name,
            pending.duration,
            &pending.target.name,
        );
    }
    ctx.tf(
        "security.rule_added",
        &[
            &ctx.tr(&format!("security.{}", pending.target.kind)),
            &pending.target.url,
            &escape_html(&pending.target.name),
            &ctx.tr(&format!("security.{rule_type}")),
            &escape_html(rule_name),
            &time_label(ctx, pending.duration),
        ],
    )
}

fn lookup_rules(ctx: &Arc<Ctx>, needle: &str) -> Vec<String> {
    let prefixes = ctx.prefixes(ctx.me_id);
    let lower = needle.to_lowercase();
    let had_prefix = prefixes
        .iter()
        .any(|prefix| lower.starts_with(&prefix.to_lowercase()));
    let mut command = lower.clone();
    if had_prefix {
        for prefix in &prefixes {
            if let Some(rest) = lower.strip_prefix(&prefix.to_lowercase()) {
                command = rest.to_owned();
                break;
            }
        }
    }

    let mut out = Vec::new();
    if !had_prefix {
        if let Some(module) = ctx.modules.get(needle) {
            out.push(format!("module/{}", module.name()));
        }
    }
    if ctx
        .modules
        .find_command(&command)
        .into_iter()
        .next()
        .is_some()
    {
        out.push(format!("command/{command}"));
    }
    out
}

fn extract_time(args: &[String]) -> i64 {
    for (suffix, quantifier) in [("d", 86_400), ("h", 3_600), ("m", 60), ("s", 1)] {
        for arg in args {
            if let Some(value) = arg.strip_suffix(suffix) {
                if !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()) {
                    return value.parse::<i64>().unwrap_or(0) * quantifier;
                }
            }
        }
    }
    0
}

async fn describe_rules(ctx: &Arc<Ctx>) -> String {
    let mut entries = Vec::new();
    for (kind, marker) in [("tsec_chat", "👥"), ("tsec_user", "👤")] {
        for rule in ctx.db.get_array(crate::security::NS, kind) {
            let target = rule.get("target").and_then(Value::as_i64).unwrap_or(0);
            let rule_type = rule
                .get("rule_type")
                .and_then(Value::as_str)
                .unwrap_or("command");
            let rule_name = rule.get("rule").and_then(Value::as_str).unwrap_or("?");
            let expires = rule.get("expires").and_then(Value::as_i64).unwrap_or(0);
            let remaining = if expires == 0 {
                0
            } else {
                expires - chrono::Utc::now().timestamp()
            };
            let name = if kind == "tsec_user" {
                user_display_name(ctx, target).await
            } else {
                target.to_string()
            };
            entries.push(format!(
                "{marker} <b><a href='tg://user?id={target}'>{name}</a> {} {} {}</b> <code>{}</code>",
                human_duration(ctx, remaining),
                ctx.tr("security.for"),
                escape_html(&ctx.tr(&format!("security.{rule_type}"))),
                escape_html(rule_name)
            ));
        }
    }

    for (name, group) in ctx.security.sgroups() {
        for rule in group
            .get("permissions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let rule_type = rule
                .get("rule_type")
                .and_then(Value::as_str)
                .unwrap_or("command");
            let rule_name = rule.get("rule").and_then(Value::as_str).unwrap_or("?");
            let expires = rule.get("expires").and_then(Value::as_i64).unwrap_or(0);
            let remaining = if expires == 0 {
                0
            } else {
                expires - chrono::Utc::now().timestamp()
            };
            entries.push(format!(
                "🔒 <code>{}</code> <b>{} {} {}</b> <code>{}</code>",
                escape_html(&name),
                human_duration(ctx, remaining),
                ctx.tr("security.for"),
                escape_html(&ctx.tr(&format!("security.{rule_type}"))),
                escape_html(rule_name)
            ));
        }
    }

    entries.join("\n")
}

pub(crate) fn security_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let owners = visible_owners(ctx);
    let groups = ctx.security.sgroups();
    let mut lines = vec![ctx.tr("security.title")];
    lines.push(format!(
        "▫️ {}: <b>{}</b>",
        ctx.tr("security.owners_label"),
        owners.len()
    ));
    lines.push(format!(
        "▫️ {}: <b>{}</b>",
        ctx.tr("security.sgroups_label"),
        groups.len()
    ));
    lines.push(format!(
        "▫️ {}: <code>{}</code> ({})",
        ctx.tr("security.mask_default"),
        ctx.security.default_mask(),
        mask_to_names(ctx.security.default_mask()).join(", ")
    ));
    lines.push(format!(
        "▫️ {}: <code>{}</code> ({})",
        ctx.tr("security.mask_bounding"),
        ctx.security.bounding_mask(),
        mask_to_names(ctx.security.bounding_mask()).join(", ")
    ));
    lines.push(String::new());
    lines.push(ctx.tr("security.card_hint"));

    let buttons = vec![
        vec![
            Button::data(ctx.tr("security.btn_owners"), b"heroku:sec:o".to_vec()),
            Button::data(ctx.tr("security.btn_groups"), b"heroku:sec:g".to_vec()),
        ],
        vec![Button::data(
            ctx.tr("security.btn_permissions"),
            b"heroku:sec:p".to_vec(),
        )],
        vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
    ];
    (lines.join("\n"), buttons)
}

pub(crate) fn permissions_card(ctx: &Arc<Ctx>, page: usize) -> (String, Vec<Vec<Button>>) {
    let mut commands = ctx
        .modules
        .all()
        .iter()
        .flat_map(|module| {
            module
                .commands()
                .into_iter()
                .map(move |command| (module.name().to_owned(), command))
        })
        .collect::<Vec<_>>();
    commands.sort_by(|left, right| {
        left.0
            .to_lowercase()
            .cmp(&right.0.to_lowercase())
            .then_with(|| left.1.name.cmp(right.1.name))
    });

    let per_page = 8usize;
    let pages = commands.len().div_ceil(per_page).max(1);
    let page = page.min(pages - 1);
    let start = page * per_page;
    let end = (start + per_page).min(commands.len());

    let mut lines = vec![ctx.tr("security.permissions_title")];
    lines.push(format!(
        "▫️ {}: <code>{}</code> ({})",
        ctx.tr("security.mask_default"),
        ctx.security.default_mask(),
        mask_to_names(ctx.security.default_mask()).join(", ")
    ));
    lines.push(format!(
        "▫️ {}: <code>{}</code> ({})",
        ctx.tr("security.mask_bounding"),
        ctx.security.bounding_mask(),
        mask_to_names(ctx.security.bounding_mask()).join(", ")
    ));
    lines.push(String::new());
    lines.push(ctx.tf(
        "security.permissions_page",
        &[&(page + 1).to_string(), &pages.to_string()],
    ));
    lines.push(ctx.tr("security.permissions_hint"));

    let mut rows = Vec::new();
    for (module, command) in &commands[start..end] {
        let mask = ctx
            .security
            .command_mask(module, command.name, command.security);
        rows.push(vec![Button::data(
            format!("🛡 {module}.{} · {mask}", command.name),
            format!("heroku:sec:pm:{module}:{}", command.name).into_bytes(),
        )]);
    }

    if pages > 1 {
        let mut navigation = Vec::new();
        if page > 0 {
            navigation.push(Button::data(
                "⬅️",
                format!("heroku:sec:p:{}", page - 1).into_bytes(),
            ));
        }
        navigation.push(Button::data(
            format!("{}/{}", page + 1, pages),
            b"heroku:sec:p".to_vec(),
        ));
        if page + 1 < pages {
            navigation.push(Button::data(
                "➡️",
                format!("heroku:sec:p:{}", page + 1).into_bytes(),
            ));
        }
        rows.push(navigation);
    }

    rows.push(vec![
        Button::data(
            ctx.tr("security.btn_default_mask"),
            b"heroku:sec:pdef".to_vec(),
        ),
        Button::data(
            ctx.tr("security.btn_bounding_mask"),
            b"heroku:sec:pbnd".to_vec(),
        ),
    ]);
    rows.push(vec![
        Button::data(ctx.tr("security.btn_back"), b"heroku:sec".to_vec()),
        Button::data("✖️ Close", b"heroku:close".to_vec()),
    ]);
    (lines.join("\n"), rows)
}

pub(crate) fn command_mask_card(
    ctx: &Arc<Ctx>,
    module_name: &str,
    command_name: &str,
) -> Option<(String, Vec<Vec<Button>>)> {
    let module = ctx.modules.get(module_name)?;
    let command = module
        .commands()
        .into_iter()
        .find(|command| command.name.eq_ignore_ascii_case(command_name))?;
    let mask = ctx
        .security
        .command_mask(module.name(), command.name, command.security);

    let mut lines = vec![ctx.tf(
        "security.command_mask_title",
        &[&escape_html(module.name()), &escape_html(command.name)],
    )];
    lines.push(format!(
        "▫️ {}: <code>{mask}</code>",
        ctx.tr("security.current_mask")
    ));
    lines.push(format!(
        "▫️ {}: {}",
        ctx.tr("security.allowed_for"),
        mask_to_names(mask)
            .iter()
            .map(|name| format!("<code>{name}</code>"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    lines.push(String::new());
    lines.push(ctx.tr("security.command_mask_hint"));

    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (name, bit) in PERMISSION_NAMES {
        row.push(Button::data(
            format!("{} {name}", if mask & bit != 0 { "✅" } else { "❌" }),
            format!("heroku:sec:pb:{}:{}:{bit}", module.name(), command.name).into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![
        Button::data(
            ctx.tr("security.btn_reset_mask"),
            format!("heroku:sec:pr:{}:{}", module.name(), command.name).into_bytes(),
        ),
        Button::data(ctx.tr("security.btn_back"), b"heroku:sec:p".to_vec()),
    ]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    Some((lines.join("\n"), rows))
}

pub(crate) fn global_mask_card(ctx: &Arc<Ctx>, bounding: bool) -> (String, Vec<Vec<Button>>) {
    let mask = if bounding {
        ctx.security.bounding_mask()
    } else {
        ctx.security.default_mask()
    };
    let mut lines = vec![ctx.tr(if bounding {
        "security.bounding_mask_title"
    } else {
        "security.default_mask_title"
    })];
    lines.push(format!(
        "▫️ {}: <code>{mask}</code>",
        ctx.tr("security.current_mask")
    ));
    lines.push(format!(
        "▫️ {}: {}",
        ctx.tr("security.allowed_for"),
        mask_to_names(mask)
            .iter()
            .map(|name| format!("<code>{name}</code>"))
            .collect::<Vec<_>>()
            .join(" ")
    ));
    lines.push(String::new());
    lines.push(ctx.tr("security.command_mask_hint"));

    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (name, bit) in PERMISSION_NAMES {
        let action = if bounding { "pbd" } else { "pd" };
        row.push(Button::data(
            format!("{} {name}", if mask & bit != 0 { "✅" } else { "❌" }),
            format!("heroku:sec:{action}:{bit}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![
        Button::data(ctx.tr("security.btn_back"), b"heroku:sec:p".to_vec()),
        Button::data("✖️ Close", b"heroku:close".to_vec()),
    ]);
    (lines.join("\n"), rows)
}

pub(crate) fn querysec_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let enabled = ctx
        .db
        .get_bool(crate::security::NS, "allow_inline_query", false);
    let state = ctx.tr(if enabled {
        "inline.querysec_enabled"
    } else {
        "inline.querysec_disabled"
    });
    let html = ctx.tf("inline.querysec_card", &[&state]);
    let buttons = vec![
        vec![Button::data(
            if enabled { "✅" } else { "❌" },
            b"heroku:sec:query".to_vec(),
        )],
        vec![Button::data(
            ctx.tr("security.btn_back"),
            b"heroku:sec".to_vec(),
        )],
    ];
    (html, buttons)
}

pub(crate) fn toggle_querysec(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let current = ctx
        .db
        .get_bool(crate::security::NS, "allow_inline_query", false);
    ctx.db
        .set(crate::security::NS, "allow_inline_query", !current);
    querysec_card(ctx)
}

pub(crate) fn toggle_command_bit(
    ctx: &Arc<Ctx>,
    module_name: &str,
    command_name: &str,
    bit: u32,
) -> Option<(String, Vec<Vec<Button>>)> {
    let module = ctx.modules.get(module_name)?;
    let command = module
        .commands()
        .into_iter()
        .find(|command| command.name.eq_ignore_ascii_case(command_name))?;
    let current = ctx
        .security
        .command_mask(module.name(), command.name, command.security);
    ctx.security
        .set_command_mask(module.name(), command.name, current ^ bit);
    command_mask_card(ctx, module.name(), command.name)
}

pub(crate) fn toggle_global_bit(
    ctx: &Arc<Ctx>,
    bounding: bool,
    bit: u32,
) -> (String, Vec<Vec<Button>>) {
    let current = if bounding {
        ctx.security.bounding_mask()
    } else {
        ctx.security.default_mask()
    };
    if bounding {
        ctx.security.set_bounding_mask(current ^ bit);
    } else {
        ctx.security.set_default_mask(current ^ bit);
    }
    global_mask_card(ctx, bounding)
}

pub(crate) fn owner_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let owners = visible_owners(ctx);
    let mut lines = vec![ctx.tr("security.owners")];
    lines.push(ctx.tr("security.owners_hint"));
    lines.push(String::new());
    for owner in &owners {
        let you = if *owner == ctx.me_id { " · you" } else { "" };
        lines.push(format!("👑 <code>{owner}</code>{you}"));
    }
    lines.push(String::new());
    lines.push(ctx.tr("security.owners_footer"));

    let mut rows = vec![vec![Button::data(
        ctx.tr("security.btn_add_owner"),
        b"heroku:sec:o:add".to_vec(),
    )]];
    for owner in owners {
        if owner == ctx.me_id {
            continue;
        }
        rows.push(vec![Button::data(
            format!("❌ {owner}"),
            format!("heroku:sec:o:rm:{owner}").into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("security.btn_back"),
        b"heroku:sec".to_vec(),
    )]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

pub(crate) fn groups_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let groups = ctx.security.sgroups();
    let mut lines = vec![ctx.tr("security.groups_title")];
    if groups.is_empty() {
        lines.push(ctx.tr("security.groups_empty"));
    } else {
        for (name, group) in &groups {
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
                "🛡 <code>{}</code> — 👤 {} · 📜 {}",
                escape_html(name),
                users,
                rules
            ));
        }
    }
    lines.push(String::new());
    lines.push(ctx.tr("security.groups_hint"));

    let mut rows = Vec::new();
    for name in groups.keys() {
        rows.push(vec![Button::data(
            format!("🛡 {name}"),
            format!("heroku:sec:g:{}", escape_callback(name)).into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("security.btn_new_group"),
        b"heroku:sec:g:new".to_vec(),
    )]);
    rows.push(vec![Button::data(
        ctx.tr("security.btn_back"),
        b"heroku:sec".to_vec(),
    )]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

pub(crate) fn group_card(ctx: &Arc<Ctx>, name: &str) -> (String, Vec<Vec<Button>>) {
    let groups = ctx.security.sgroups();
    let Some(group) = groups.get(name) else {
        let (mut html, buttons) = groups_card(ctx);
        html = format!(
            "{}\n\n{}",
            ctx.tf("security.sgroup_missing", &[&escape_html(name)]),
            html
        );
        return (html, buttons);
    };

    let mut lines = vec![ctx.tf("security.group_title", &[&escape_html(name)])];
    let users = group
        .get("users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rules = group
        .get("permissions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    lines.push(format!(
        "{}: <b>{}</b>",
        ctx.tr("security.group_users"),
        users.len()
    ));
    if users.is_empty() {
        lines.push(format!("▫️ {}", ctx.tr("security.group_no_users")));
    }
    for user in &users {
        let id = user.as_i64().unwrap_or_default();
        lines.push(format!("👤 <a href=\"tg://user?id={id}\">{id}</a>"));
    }
    lines.push(String::new());
    lines.push(format!(
        "{}: <b>{}</b>",
        ctx.tr("security.group_rules"),
        rules.len()
    ));
    if rules.is_empty() {
        lines.push(format!("▫️ {}", ctx.tr("security.group_no_rules")));
    }
    for rule in &rules {
        lines.push(format!(
            "📜 <code>{}/{}</code>",
            escape_html(rule.get("rule_type").and_then(Value::as_str).unwrap_or("?")),
            escape_html(rule.get("rule").and_then(Value::as_str).unwrap_or("?"))
        ));
    }

    let encoded = escape_callback(name);
    let mut rows = vec![
        vec![
            Button::data(
                ctx.tr("security.btn_add_user"),
                format!("heroku:sec:g:{encoded}:u").into_bytes(),
            ),
            Button::data(
                ctx.tr("security.btn_add_rule"),
                format!("heroku:sec:g:{encoded}:r").into_bytes(),
            ),
        ],
        vec![Button::data(
            ctx.tr("security.btn_delete_group"),
            format!("heroku:sec:g:{encoded}:d").into_bytes(),
        )],
    ];
    // Removing users is a one-tap action; keep the buttons compact.
    for user in &users {
        let id = user.as_i64().unwrap_or_default();
        rows.push(vec![Button::data(
            format!("❌ {}", id),
            format!("heroku:sec:g:{encoded}:rm:{id}").into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("security.btn_back"),
        b"heroku:sec:g".to_vec(),
    )]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

pub(crate) async fn add_owner_pending(ctx: &Arc<Ctx>, raw: &str) -> (String, Vec<Vec<Button>>) {
    let Some(user) = resolve_inline_user(ctx, raw).await else {
        let (mut html, buttons) = owner_card(ctx);
        html = format!("{}\n\n{}", ctx.tr("security.inline_bad_user"), html);
        return (html, buttons);
    };
    let html = if ctx.security.add_owner(user) {
        ctx.tf("security.added", &[&user.to_string()])
    } else {
        ctx.tr("security.already")
    };
    let (card, buttons) = owner_card(ctx);
    (format!("{html}\n\n{card}"), buttons)
}

pub(crate) async fn create_group_pending(ctx: &Arc<Ctx>, raw: &str) -> (String, Vec<Vec<Button>>) {
    let name = raw.trim();
    if name.is_empty() || !name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
        let (mut html, buttons) = groups_card(ctx);
        html = format!("{}\n\n{}", ctx.tr("security.inline_bad_group"), html);
        return (html, buttons);
    }
    let mut groups = ctx.security.sgroups();
    if groups.contains_key(name) {
        let (html, buttons) = group_card(ctx, name);
        return (format!("{}\n\n{html}", ctx.tr("security.already")), buttons);
    }
    groups.insert(
        name.to_owned(),
        serde_json::json!({"users": [], "permissions": []}),
    );
    ctx.security.save_sgroups(groups);
    group_card(ctx, name)
}

pub(crate) async fn add_group_user_pending(
    ctx: &Arc<Ctx>,
    group: &str,
    raw: &str,
) -> (String, Vec<Vec<Button>>) {
    let Some(user) = resolve_inline_user(ctx, raw).await else {
        let (mut html, buttons) = group_card(ctx, group);
        html = format!("{}\n\n{}", ctx.tr("security.inline_bad_user"), html);
        return (html, buttons);
    };
    if !ctx.security.sgroups().contains_key(group) {
        return groups_card(ctx);
    }
    let html = if ctx.security.add_sgroup(group, user) {
        ctx.tf("security.rule_added_sgroup", &[&escape_html(group)])
    } else {
        ctx.tr("security.not_present")
    };
    let (card, buttons) = group_card(ctx, group);
    (format!("{html}\n\n{card}"), buttons)
}

pub(crate) async fn add_group_rule_pending(
    ctx: &Arc<Ctx>,
    group: &str,
    raw: &str,
) -> (String, Vec<Vec<Button>>) {
    if !ctx.security.sgroups().contains_key(group) {
        return groups_card(ctx);
    }
    let mut parts = raw.split_whitespace();
    let (Some(rule_type), Some(rule)) = (parts.next(), parts.next()) else {
        let (mut html, buttons) = group_card(ctx, group);
        html = format!("{}\n\n{}", ctx.tr("security.inline_bad_rule"), html);
        return (html, buttons);
    };
    if !matches!(rule_type, "command" | "module") {
        let (mut html, buttons) = group_card(ctx, group);
        html = format!("{}\n\n{}", ctx.tr("security.inline_bad_rule"), html);
        return (html, buttons);
    }
    let html = if ctx.security.add_group_rule(group, rule_type, rule) {
        ctx.tf("security.rule_added_sgroup", &[&escape_html(group)])
    } else {
        ctx.tr("security.not_present")
    };
    let (card, buttons) = group_card(ctx, group);
    (format!("{html}\n\n{card}"), buttons)
}

fn visible_owners(ctx: &Arc<Ctx>) -> Vec<i64> {
    let mut owners = ctx.security.owners();
    if !owners.contains(&ctx.me_id) {
        owners.push(ctx.me_id);
    }
    owners
}

async fn resolve_inline_user(ctx: &Arc<Ctx>, raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if let Ok(id) = raw.parse::<i64>() {
        return Some(id);
    }
    ctx.resolve(raw)
        .await
        .map(|peer| super::common::peer_id(&peer))
}

/// Security group names only contain `[A-Za-z0-9_]`, so callback encoding is a no-op.
/// It is kept as a helper so a future name policy cannot silently break callbacks.
fn escape_callback(name: &str) -> String {
    name.to_owned()
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
