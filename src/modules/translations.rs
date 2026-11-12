// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Internal language management.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::Msg;

pub struct Translations;

#[async_trait]
impl Module for Translations {
    fn name(&self) -> &'static str {
        "Translations"
    }

    fn description(&self) -> &'static str {
        "Processes internal translations"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("setlang", "Change the userbot language").usage("<en|ru>"),
            CommandDef::new("dllangpack", "Manage language packs").usage("[language]"),
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
            "setlang" => {
                let lang = args
                    .get(0)
                    .map(|raw| raw.to_lowercase())
                    .unwrap_or_default();
                if lang.is_empty() {
                    message
                        .reply_html(format!(
                            "{}\n{}",
                            ctx.tf("translations.current", &[&ctx.translator.lang()]),
                            ctx.tf(
                                "translations.available",
                                &[&ctx.translator.available().join(", ")]
                            )
                        ))
                        .await?;
                    return Ok(());
                }
                if !ctx.translator.available().contains(&lang.as_str()) {
                    message.reply_html(ctx.tr("translations.unknown")).await?;
                    return Ok(());
                }
                ctx.translator.set_lang(&lang);
                ctx.db.set("heroku.translations", "lang", lang.as_str());
                message
                    .reply_html(ctx.tf("translations.set", &[&lang]))
                    .await?;
            }
            "dllangpack" => {
                message
                    .reply_html(format!(
                        "{}\n<code>{}</code>",
                        ctx.tr("translations.available"),
                        ctx.translator.available().join(" ")
                    ))
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(Translations))
}
