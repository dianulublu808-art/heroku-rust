// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Internal language management: `.setlang`, `.dllangpack` and the inline
//! language picker (official packs, meme packs and downloaded pack URLs).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use grammers_client::message::{Button, InputMessage, ReplyMarkup};
use grammers_client::update::CallbackQuery;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};
use crate::translations::{is_known_lang, is_url, parse_langs, MEME_LANGUAGES, OFFICIAL_LANGUAGES};

const NS: &str = "heroku.translations";

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
            CommandDef::new("setlang", "Change the userbot language").usage("[language|url ...]"),
            CommandDef::new("dllangpack", "Add an external translation pack")
                .aliases(&["dllangpackcmd"])
                .usage("<url>"),
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
            "setlang" => setlang(ctx, message, args).await,
            "dllangpack" => dllangpack(ctx, message, args).await,
            _ => Ok(()),
        }
    }
}

async fn setlang(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
    let raw = args.raw.trim();
    if raw.is_empty() {
        let (html, buttons) = language_card(ctx, false);
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
            .reply_html(ctx.tf("translations.available", &[&ctx.translator.langs()]))
            .await?;
        return Ok(());
    }

    // Reject unknown tokens unless they are pack URLs, exactly like Python.
    for token in raw.split_whitespace() {
        let normalized = if is_url(token) {
            token.to_owned()
        } else {
            crate::translations::canonical_lang(token)
        };
        if !is_url(token) && !is_known_lang(&normalized) {
            message
                .reply_html(ctx.tr("translations.incorrect_language"))
                .await?;
            return Ok(());
        }
    }

    let langs = parse_langs(raw);
    if load_langs(ctx, &langs).await.is_err() {
        message
            .reply_html(ctx.tr("translations.check_pack"))
            .await?;
        return Ok(());
    }

    let joined = langs.join(" ");
    ctx.translator.set_lang(&joined);
    ctx.db.set(NS, "lang", joined.clone());

    let flags: String = langs
        .iter()
        .map(|lang| {
            if is_url(lang) {
                "📁".to_owned()
            } else {
                flag(lang)
            }
        })
        .collect();
    let mut response = ctx.tf("translations.lang_saved", &[&flags]);
    if langs
        .iter()
        .any(|lang| !is_url(lang) && !is_known_lang(lang))
    {
        response.push_str("\n\n");
        response.push_str(&ctx.tr("translations.not_official"));
    }
    message.reply_html(response).await?;
    Ok(())
}

async fn dllangpack(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
    let url = args.raw.trim();
    if !is_url(url) {
        message.reply_html(ctx.tr("translations.check_url")).await?;
        return Ok(());
    }

    let mut langs = parse_langs(&ctx.translator.langs());
    langs.retain(|token| !is_url(token) || token != url);
    let mut with_url = langs.clone();
    with_url.push(url.to_owned());

    if load_langs(ctx, &with_url).await.is_err() {
        message
            .reply_html(ctx.tr("translations.check_pack"))
            .await?;
        return Ok(());
    }

    let joined = with_url.join(" ");
    ctx.translator.set_lang(&joined);
    ctx.db.set(NS, "lang", joined);
    message
        .reply_html(ctx.tr("translations.pack_saved"))
        .await?;
    Ok(())
}

/// Fetch every URL in `langs`, merge the packs and persist the URL list so it
/// can be restored on the next start. Non-URL tokens are ignored.
async fn load_langs(ctx: &Arc<Ctx>, langs: &[String]) -> Result<usize> {
    let mut loaded = 0;
    for url in langs.iter().filter(|token| is_url(token)) {
        let raw = fetch_pack(url).await?;
        loaded += ctx.translator.merge_external(url, &raw);
    }
    persist_urls(ctx, langs);
    Ok(loaded)
}

fn persist_urls(ctx: &Arc<Ctx>, langs: &[String]) {
    let urls: Vec<serde_json::Value> = langs
        .iter()
        .filter(|token| is_url(token))
        .map(|url| serde_json::Value::String(url.clone()))
        .collect();
    ctx.db.set(NS, "external", serde_json::Value::Array(urls));
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
                let count = translator.merge_external(url, &raw);
                tracing::info!("Loaded {count} strings from language pack {url}");
            }
            Err(err) => tracing::warn!("Failed to load language pack {url}: {err:#}"),
        }
    }
}

fn flag(code: &str) -> String {
    match code {
        "en" => "🇬🇧",
        "ru" => "🇷🇺",
        "uk" => "🇺🇦",
        "de" => "🇩🇪",
        "ja" => "🇯🇵",
        "leet" | "uwu" | "tiktok" | "neofit" => "🏴‍☠️",
        _ => "🌐",
    }
    .to_owned()
}

fn language_name(code: &str) -> &'static str {
    match code {
        "en" => "English",
        "ru" => "Русский",
        "uk" => "Українська",
        "de" => "Deutsch",
        "ja" => "日本語",
        "leet" => "1337",
        "uwu" => "UwU",
        "tiktok" => "TikTokKid",
        "neofit" => "Neofit",
        _ => "Language",
    }
}

fn language_rows(ctx: &Arc<Ctx>, codes: &[&str], current: &str) -> Vec<Vec<Button>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for code in codes {
        let marker = if *code == current { " ✅" } else { "" };
        row.push(Button::data(
            format!("{} {}{}", flag(code), language_name(code), marker),
            format!("heroku:lang/{code}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    let _ = ctx;
    rows
}

/// First page of the picker: official packs.
pub(crate) fn language_card(ctx: &Arc<Ctx>, meme: bool) -> (String, Vec<Vec<Button>>) {
    let current = ctx.translator.lang();
    let mut lines = vec![ctx.tr("translations.choose_language")];
    lines.push(ctx.tf("translations.current", &[&current]));

    let codes: &[&str] = if meme {
        MEME_LANGUAGES
    } else {
        OFFICIAL_LANGUAGES
    };
    let mut rows = language_rows(ctx, codes, &current);
    let switch = if meme {
        Button::data(
            ctx.tr("translations.off_langs"),
            b"heroku:langs/official".to_vec(),
        )
    } else {
        Button::data(
            ctx.tr("translations.meme_langs"),
            b"heroku:langs/meme".to_vec(),
        )
    };
    rows.push(vec![switch]);
    rows.push(vec![Button::data(
        ctx.tr("translations.downloaded_langs"),
        b"heroku:langs/downloaded".to_vec(),
    )]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

/// Downloaded pack list; each entry opens a confirmation page.
pub(crate) fn downloaded_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let urls = downloaded(ctx);
    if urls.is_empty() {
        let rows = vec![
            vec![Button::data(
                ctx.tr("translations.btn_back"),
                b"heroku:langs/official".to_vec(),
            )],
            vec![Button::data("✖️ Close", b"heroku:close".to_vec())],
        ];
        return (ctx.tr("translations.no_downloaded"), rows);
    }

    let mut rows = Vec::new();
    for url in &urls {
        rows.push(vec![Button::data(
            url.rsplit('/').next().unwrap_or(url).to_owned(),
            format!("heroku:langs/confirm/{url}").into_bytes(),
        )]);
    }
    rows.push(vec![Button::data(
        ctx.tr("translations.btn_back"),
        b"heroku:langs/official".to_vec(),
    )]);
    (ctx.tr("translations.choose_downloaded"), rows)
}

pub(crate) fn confirm_card(ctx: &Arc<Ctx>, url: &str) -> (String, Vec<Vec<Button>>) {
    (
        ctx.tr("translations.confirm_lang"),
        vec![
            vec![Button::data(
                ctx.tr("translations.btn_yes"),
                format!("heroku:langs/set/{url}").into_bytes(),
            )],
            vec![Button::data(
                ctx.tr("translations.btn_cancel"),
                b"heroku:langs/cancel".to_vec(),
            )],
            vec![Button::data(
                ctx.tr("translations.btn_delete"),
                format!("heroku:langs/delete/{url}").into_bytes(),
            )],
        ],
    )
}

fn downloaded(ctx: &Arc<Ctx>) -> Vec<String> {
    parse_langs(&ctx.translator.langs())
        .into_iter()
        .filter(|token| is_url(token))
        .collect()
}

/// Handle callbacks under `heroku:lang/` and `heroku:langs/`.
pub(crate) async fn handle_inline_callback(
    ctx: &Arc<Ctx>,
    call: &CallbackQuery,
    data: &str,
) -> Result<bool> {
    // Each callback may only be answered once, so the toast text and the edit
    // are always sent in a single `answer().edit(...)` call.
    async fn show(
        call: &CallbackQuery,
        toast: Option<String>,
        html: String,
        buttons: Vec<Vec<Button>>,
    ) {
        let message = InputMessage::new()
            .html(html)
            .reply_markup(ReplyMarkup::from_buttons(&buttons));
        let answer = call.answer();
        let answer = match toast {
            Some(text) => answer.text(text),
            None => answer,
        };
        let _ = answer.edit(message).await;
    }

    if let Some(code) = data.strip_prefix("heroku:lang/") {
        let code = crate::translations::canonical_lang(code);
        if !is_known_lang(&code) {
            let _ = call
                .answer()
                .alert(ctx.tr("translations.incorrect_language"))
                .send()
                .await;
            return Ok(true);
        }
        ctx.translator.set_lang(&code);
        ctx.db.set(NS, "lang", code.clone());
        let (html, buttons) = language_card(ctx, false);
        show(
            call,
            Some(ctx.tf("translations.lang_saved", &[&flag(&code)])),
            html,
            buttons,
        )
        .await;
        return Ok(true);
    }

    if data == "heroku:langs/meme" {
        let (html, buttons) = language_card(ctx, true);
        show(call, None, html, buttons).await;
        return Ok(true);
    }

    if data == "heroku:langs/official" {
        let (html, buttons) = language_card(ctx, false);
        show(call, None, html, buttons).await;
        return Ok(true);
    }

    if data == "heroku:langs/downloaded" {
        let (html, buttons) = downloaded_card(ctx);
        show(call, None, html, buttons).await;
        return Ok(true);
    }

    if let Some(url) = data.strip_prefix("heroku:langs/confirm/") {
        let (html, buttons) = confirm_card(ctx, url);
        show(call, None, html, buttons).await;
        return Ok(true);
    }

    if let Some(url) = data.strip_prefix("heroku:langs/set/") {
        let mut langs: Vec<String> = parse_langs(&ctx.translator.langs())
            .into_iter()
            .filter(|token| !is_url(token) || token != url)
            .collect();
        langs.push(url.to_owned());
        if load_langs(ctx, &langs).await.is_err() {
            let _ = call
                .answer()
                .alert(ctx.tr("translations.check_pack"))
                .send()
                .await;
            return Ok(true);
        }
        ctx.translator.set_lang(&langs.join(" "));
        ctx.db.set(NS, "lang", langs.join(" "));
        let label = url.rsplit('/').next().unwrap_or(url);
        let (html, buttons) = language_card(ctx, false);
        show(
            call,
            Some(ctx.tf("translations.lang_saved", &[&escape_html(label)])),
            html,
            buttons,
        )
        .await;
        return Ok(true);
    }

    if let Some(url) = data.strip_prefix("heroku:langs/delete/") {
        let langs: Vec<String> = parse_langs(&ctx.translator.langs())
            .into_iter()
            .filter(|token| !is_url(token) || token != url)
            .collect();
        let joined = if langs.is_empty() {
            "en".to_owned()
        } else {
            langs.join(" ")
        };
        ctx.translator.set_lang(&joined);
        ctx.db.set(NS, "lang", joined.clone());
        persist_urls(ctx, &langs);
        let (html, buttons) = downloaded_card(ctx);
        show(
            call,
            Some(ctx.tr("translations.lang_deleted")),
            html,
            buttons,
        )
        .await;
        return Ok(true);
    }

    if data == "heroku:langs/cancel" {
        let (html, buttons) = language_card(ctx, false);
        show(
            call,
            Some(ctx.tr("translations.action_cancelled")),
            html,
            buttons,
        )
        .await;
        return Ok(true);
    }

    Ok(false)
}

inventory::submit! {
    ModuleFactory(|| Box::new(Translations))
}
