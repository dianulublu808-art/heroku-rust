// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.config` — inspect and change module configuration.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct HerokuConfigMod;

#[async_trait]
impl Module for HerokuConfigMod {
    fn name(&self) -> &'static str {
        "HerokuConfigMod"
    }

    fn description(&self) -> &'static str {
        "Configure modules"
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![ConfigSpec::new(
            "cfg_emoji",
            "Emoji shown in the configuration menu",
            ConfigDefault::Text("🪐"),
            ConfigKind::Text,
        )]
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("config", "Show or change module configuration")
                .aliases(&["cfg", "fcfg"])
                .usage("[module] [key] [value]"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        let Some(module_name) = args.get(0) else {
            let user_id = message.sender_id().unwrap_or(ctx.me_id);
            if ctx.inline.send_config_page(ctx, user_id).await.is_ok() {
                return Ok(());
            }

            let mut lines = vec!["⚙️ <b>Modules with configuration</b>".to_owned()];
            for module in ctx.modules.all() {
                let specs = module.config_spec();
                if specs.is_empty() {
                    continue;
                }
                lines.push(format!(
                    "▪️ <code>{}</code> — <i>{}</i> <code>[{}]</code>",
                    escape_html(module.name()),
                    escape_html(module.description()),
                    specs.len()
                ));
            }
            message.reply_html(lines.join("\n")).await?;
            return Ok(());
        };

        let Some(module) = ctx.modules.get(module_name) else {
            message.reply_html(ctx.tr("help.no_module")).await?;
            return Ok(());
        };
        let specs = module.config_spec();
        if specs.is_empty() {
            message.reply_html(ctx.tr("config.empty")).await?;
            return Ok(());
        }

        let Some(key) = args.get(1) else {
            let user_id = message.sender_id().unwrap_or(ctx.me_id);
            if ctx
                .inline
                .send_config_module(ctx, user_id, module.name())
                .await
                .is_ok()
            {
                return Ok(());
            }

            let mut lines = vec![ctx.tf("config.title", &[&escape_html(module.name())])];
            for spec in &specs {
                lines.push(format!(
                    "▫️ <code>{}</code> = {}\n   <i>{}</i>",
                    escape_html(spec.key),
                    super::common::show_value(&load_value(ctx, module.name(), spec)),
                    escape_html(spec.description)
                ));
            }
            message.reply_html(lines.join("\n")).await?;
            return Ok(());
        };

        let Some(spec) = specs.iter().find(|spec| spec.key.eq_ignore_ascii_case(key)) else {
            message.reply_html(ctx.tr("config.not_found")).await?;
            return Ok(());
        };

        let Some(_) = args.get(2) else {
            message
                .reply_html(format!(
                    "▫️ <code>{}</code> = {}\n<i>{}</i>",
                    escape_html(spec.key),
                    super::common::show_value(&load_value(ctx, module.name(), spec)),
                    escape_html(spec.description)
                ))
                .await?;
            return Ok(());
        };

        let full = args.list[2..].join(" ");
        let value = match parse_value(spec, &full) {
            Some(value) => value,
            None => {
                message.reply_html(ctx.tr("config.invalid")).await?;
                return Ok(());
            }
        };
        ctx.db.set(module.name(), spec.key, value.clone());
        message
            .reply_html(ctx.tf(
                "config.set",
                &[
                    &escape_html(spec.key),
                    &escape_html(value.as_str().unwrap_or(&value.to_string())),
                ],
            ))
            .await?;
        Ok(())
    }
}

fn load_value(ctx: &Arc<Ctx>, module: &str, spec: &ConfigSpec) -> Value {
    ctx.db
        .get_value(module, spec.key)
        .unwrap_or_else(|| spec.default.to_value())
}

fn parse_value(spec: &ConfigSpec, raw: &str) -> Option<Value> {
    match spec.kind {
        ConfigKind::Bool => super::common::parse_bool(raw).map(Value::Bool),
        ConfigKind::Int => raw.trim().parse::<i64>().ok().map(Value::from),
        ConfigKind::Float => raw.trim().parse::<f64>().ok().map(Value::from),
        ConfigKind::Text | ConfigKind::Hidden => Some(Value::String(raw.to_owned())),
        ConfigKind::Choice(choices) => {
            let value = choices
                .iter()
                .find(|choice| choice.eq_ignore_ascii_case(raw.trim()))?;
            Some(Value::String((*value).to_owned()))
        }
        ConfigKind::List => Some(Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| Value::String(item.to_owned()))
                .collect(),
        )),
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuConfigMod))
}
