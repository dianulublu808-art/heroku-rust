// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Internal language management.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use grammers_client::message::Button;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

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
            CommandDef::new("setlang", "Change the userbot language")
                .usage("<en|ru|uk|de|ja|leet|uwu|tiktok|neofit>"),
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
                if let Some(url) = args.get(0) {
                    if url.starts_with("http://") || url.starts_with("https://") {
                        match fetch_pack(url).await {
                            Ok(raw) => {
                                let count = ctx.translator.merge_external(&raw);
                                if count == 0 {
                                    message
                                        .reply_html(ctx.tr("translations.check_pack"))
                                        .await?;
                                    return Ok(());
                                }
                                let mut external =
                                    ctx.db.get_string_list("heroku.translations", "external");
                                if !external.iter().any(|item| item == url) {
                                    external.push(url.to_owned());
                                    ctx.db.set(
                                        "heroku.translations",
                                        "external",
                                        serde_json::Value::Array(
                                            external
                                                .into_iter()
                                                .map(serde_json::Value::String)
                                                .collect(),
                                        ),
                                    );
                                }
                                message
                                    .reply_html(ctx.tf(
                                        "translations.pack_saved",
                                        &[&escape_html(url.rsplit('/').next().unwrap_or(url))],
                                    ))
                                    .await?;
                            }
                            Err(err) => {
                                tracing::warn!("Failed to download language pack: {err:#}");
                                message
                                    .reply_html(ctx.tr("translations.check_pack"))
                                    .await?;
                            }
                        }
                        return Ok(());
                    }
                }
                let lang = args
                    .get(0)
                    .map(crate::translations::canonical_lang)
                    .unwrap_or_default();
                if lang.is_empty() {
                    let (html, buttons) = language_card(ctx);
                    let user_id = message.sender_id().unwrap_or(ctx.me_id);
                    if ctx
                        .inline
                        .send_to_user(ctx, user_id, html, buttons)
                        .await
                        .is_ok()
                    {
                        return Ok(());
                    }
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
                let Some(url) = args.get(0) else {
                    let mut lines = vec![ctx.tf(
                        "translations.available",
                        &[&ctx.translator.available().join(" ")],
                    )];
                    let external = ctx.db.get_string_list("heroku.translations", "external");
                    if !external.is_empty() {
                        lines.push(String::new());
                        lines.push(ctx.tr("translations.downloaded"));
                        for url in external {
                            lines.push(format!(
                                "▫️ <a href=\"{}\">{}</a>",
                                escape_html(&url),
                                escape_html(url.rsplit('/').next().unwrap_or(&url))
                            ));
                        }
                    }
                    lines.push(String::new());
                    lines.push(ctx.tr("translations.dllangpack_hint"));
                    message.reply_html(lines.join("\n")).await?;
                    return Ok(());
                };

                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    message
                        .reply_html(ctx.tr("translations.pack_url_invalid"))
                        .await?;
                    return Ok(());
                }

                match fetch_pack(url).await {
                    Ok(raw) => {
                        let count = ctx.translator.merge_external(&raw);
                        if count == 0 {
                            message
                                .reply_html(ctx.tr("translations.check_pack"))
                                .await?;
                            return Ok(());
                        }
                        let mut external =
                            ctx.db.get_string_list("heroku.translations", "external");
                        if !external.iter().any(|item| item == url) {
                            external.push(url.to_owned());
                            ctx.db.set(
                                "heroku.translations",
                                "external",
                                serde_json::Value::Array(
                                    external
                                        .into_iter()
                                        .map(serde_json::Value::String)
                                        .collect(),
                                ),
                            );
                        }
                        message
                            .reply_html(ctx.tf(
                                "translations.pack_saved",
                                &[&escape_html(url.rsplit('/').next().unwrap_or(url))],
                            ))
                            .await?;
                    }
                    Err(err) => {
                        tracing::warn!("Failed to download language pack: {err:#}");
                        message
                            .reply_html(ctx.tr("translations.check_pack"))
                            .await?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

async fn fetch_pack(url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("Heroku/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTP client")?;
    let response = client
        .get(url)
        .send()
        .await
        .context("language pack request failed")?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    response
        .text()
        .await
        .context("failed to read language pack")
}

/// Load external language packs from persisted URLs at startup. Failures are
/// logged and skipped so a dead link never blocks the userbot.
pub async fn load_external_packs(translator: &crate::translations::Translator, urls: &[String]) {
    for url in urls {
        match fetch_pack(url).await {
            Ok(raw) => {
                let count = translator.merge_external(&raw);
                tracing::info!("Loaded {count} strings from language pack {url}");
            }
            Err(err) => tracing::warn!("Failed to load language pack {url}: {err:#}"),
        }
    }
}

pub(crate) fn language_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let current = ctx.translator.lang();
    let mut lines = vec![ctx.tf("translations.choose", &[&current])];
    lines.push(String::new());
    lines.push(ctx.tr("translations.choose_hint"));

    let flags = [
        ("en", "🇬🇧"),
        ("ru", "🇷🇺"),
        ("uk", "🇺🇦"),
        ("de", "🇩🇪"),
        ("ja", "🇯🇵"),
        ("leet", "🏴‍☠️"),
        ("uwu", "🥟"),
        ("tiktok", "🎵"),
        ("neofit", "🕶"),
    ];
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for code in ctx.translator.available() {
        let flag = flags
            .iter()
            .find(|(name, _)| *name == code)
            .map(|(_, flag)| *flag)
            .unwrap_or("🌐");
        let marker = if code == current { "✅" } else { "" };
        row.push(Button::data(
            format!("{flag} {code} {marker}").trim().to_owned(),
            format!("heroku:lang/{code}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

inventory::submit! {
    ModuleFactory(|| Box::new(Translations))
}
