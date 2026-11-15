// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.tr` — translate text or a replied message.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct Translator;

#[async_trait]
impl Module for Translator {
    fn name(&self) -> &'static str {
        "Translator"
    }

    fn description(&self) -> &'static str {
        "Translates text"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![CommandDef::new("tr", "Translate text or a replied message").usage("[language] <text>")]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "provider",
                "Translation provider",
                ConfigDefault::Text("google"),
                ConfigKind::Choice(&["google"]),
            ),
            ConfigSpec::new(
                "only_text",
                "Return only the translated text without the template",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        let raw = args.raw.trim();
        let default_lang = ctx.tr("translate.language");

        let mut lang = default_lang.clone();
        let mut text: Option<String> = None;
        if !raw.is_empty() {
            let (first, rest) = super::common::split_once(raw);
            if first.chars().count() == 2 {
                lang = first.to_lowercase();
                if !rest.is_empty() {
                    text = Some(rest.to_owned());
                }
            } else {
                text = Some(raw.to_owned());
            }
        }

        if text.is_none() {
            let Some(reply) = message.get_reply().await? else {
                message.reply_html(ctx.tr("translate.no_args")).await?;
                return Ok(());
            };
            let body = reply.text().trim().to_owned();
            if body.is_empty() {
                message.reply_html(ctx.tr("translate.no_args")).await?;
                return Ok(());
            }
            text = Some(body);
        }

        let text = text.unwrap_or_default();
        let translated = match translate(&text, &lang).await {
            Ok(text) => text,
            Err(err) => {
                tracing::warn!("Translation failed: {err:#}");
                message.reply_html(ctx.tr("translate.error")).await?;
                return Ok(());
            }
        };

        let only_text = ctx.db.get_bool("Translator", "only_text", false);
        let response = if only_text {
            escape_html(&translated)
        } else {
            ctx.tf("translate.result", &[&escape_html(&translated)])
        };
        message.reply_html(response).await?;
        Ok(())
    }
}

async fn translate(text: &str, lang: &str) -> Result<String> {
    let response = reqwest::Client::new()
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[
            ("client", "gtx"),
            ("sl", "auto"),
            ("tl", lang),
            ("dt", "t"),
            ("q", text),
        ])
        .header(
            "User-Agent",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/122 Safari/537.36",
        )
        .send()
        .await?
        .error_for_status()?;
    let value: Value = response.json().await?;
    let translated = value
        .get(0)
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|part| part.get(0).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|text| !text.is_empty())
        .ok_or_else(|| anyhow::anyhow!("unexpected response"))?;
    Ok(translated)
}

inventory::submit! {
    ModuleFactory(|| Box::new(Translator))
}
