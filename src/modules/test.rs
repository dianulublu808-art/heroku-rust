// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Ping, logs and self-testing helpers.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::{Button, InputMessage, InputRichMessage, ReplyMarkup};
use grammers_client::update::CallbackQuery;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

const NS: &str = "TestMod";
const DEFAULT_PING: &str =
    "⚡️ <b>𝙿𝚒𝚗𝚐: </b><code>{ping}</code><b> 𝚖𝚜 </b>\n🕓<b> 𝚄𝚙𝚝𝚒𝚖𝚎: </b><code>{uptime}</code>";

const LEVELS: &[(&str, i32)] = &[
    ("🚫 Critical", 50),
    ("🚫 Error", 40),
    ("⚠️ Warning", 30),
    ("ℹ️ Info", 20),
    ("⚠️ Debug", 10),
    ("🧑‍💻 All", 0),
];

pub struct TestMod;

#[async_trait]
impl Module for TestMod {
    fn name(&self) -> &'static str {
        "TestMod"
    }

    fn description(&self) -> &'static str {
        "Perform operations based on userbot self-testing"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("ping", "Measure the userbot latency"),
            CommandDef::new("id", "Show user, chat and message identifiers"),
            CommandDef::new("logs", "Dump logs, optionally filtered by module")
                .usage("[modules] [-f] [level]"),
            CommandDef::new("clearlogs", "Clear the log buffer"),
            CommandDef::new("suspend", "Pause command handling").usage("<seconds>"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "force_send_all",
                "Send all logs to the log chat regardless of level",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "tglog_level",
                "Minimum level forwarded to the Telegram log chat",
                ConfigDefault::Text("ERROR"),
                ConfigKind::Choice(&[
                    "ALL", "DEBUG", "INFO", "WARNING", "ERROR", "CRITICAL", "DISABLE",
                ]),
            ),
            ConfigSpec::new(
                "ignore_common",
                "Ignore common log messages in the log chat",
                ConfigDefault::Bool(true),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "disable_internet_warn",
                "Disable internet connection warnings",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "custom_message",
                "Custom .ping template. Placeholders: {ping}, {uptime}, {ping_hint}, \
                 {hostname}, {user}, {platform}, {img}",
                ConfigDefault::Text(DEFAULT_PING),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "hint",
                "Optional hint shown under the ping from time to time",
                ConfigDefault::Text(""),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "ping_emoji",
                "Emoji shown while the ping is being measured",
                ConfigDefault::Text("🪐"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "banner_url",
                "URL to an image shown above the ping",
                ConfigDefault::Text(""),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "quote_media",
                "Show the banner as a link preview instead of a photo",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "rich_mode",
                "Render .ping as a Telegram rich message",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "invert_media",
                "Show the banner below the text",
                ConfigDefault::Bool(false),
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
            "ping" => ping(ctx, message).await?,
            "id" => {
                let sender = message
                    .sender_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                let kind = if message.is_private() {
                    ctx.tr("test.type_private")
                } else if message.is_channel() {
                    ctx.tr("test.type_channel")
                } else {
                    ctx.tr("test.type_group")
                };
                message
                    .reply_html(format!(
                        "{title}\n{user}\n{chat}\n{message_id}\n{kind}",
                        title = ctx.tr("test.ids_title"),
                        user = ctx.tf("test.id_user", &[&sender]),
                        chat = ctx.tf("test.id_chat", &[&message.chat_id().to_string()]),
                        message_id = ctx.tf("test.id_message", &[&message.id().to_string()]),
                        kind = ctx.tf("test.id_type", &[&kind]),
                    ))
                    .await?;
            }
            "logs" => logs_command(ctx, message, args).await?,
            "clearlogs" => {
                ctx.log_buffer.clear();
                message.reply_html(ctx.tr("test.logs_cleared")).await?;
            }
            "suspend" => {
                let raw = args.raw.trim();
                let seconds = match raw.parse::<f64>() {
                    Ok(value) if value <= 86_400.0 * 365.0 * 100.0 => value.max(0.0),
                    _ => {
                        message
                            .reply_html(ctx.tr("test.suspend_invalid_time"))
                            .await?;
                        return Ok(());
                    }
                };
                message
                    .reply_html(ctx.tf("test.suspended", &[&format!("{seconds:.1}")]))
                    .await?;
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            }
            _ => {}
        }
        Ok(())
    }
}

async fn ping(ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
    let started = Instant::now();
    let emoji = ctx.db.get_str(NS, "ping_emoji", "🪐");
    let response = message.respond_html(emoji).await?;

    let banner = ctx.db.get_str(NS, "banner_url", "");
    let rich = ctx.db.get_bool(NS, "rich_mode", false);
    let quote = ctx.db.get_bool(NS, "quote_media", false);
    let invert = ctx.db.get_bool(NS, "invert_media", false);
    let custom = ctx.db.get_str(NS, "custom_message", DEFAULT_PING);
    let hint = ctx.db.get_str(NS, "hint", "");

    let mut data: HashMap<String, String> = HashMap::new();
    data.insert(
        "ping".to_owned(),
        format!("{:.3}", started.elapsed().as_secs_f64() * 1000.0),
    );
    data.insert("uptime".to_owned(), ctx.uptime());
    data.insert(
        "ping_hint".to_owned(),
        if hint.is_empty() || rand::random::<u8>() % 3 != 0 {
            String::new()
        } else {
            hint.clone()
        },
    );
    data.insert("hostname".to_owned(), super::heroku_info::hostname());
    data.insert("user".to_owned(), super::heroku_info::current_user());
    data.insert("platform".to_owned(), super::heroku_info::platform_name());
    data.insert(
        "img".to_owned(),
        if rich && !banner.is_empty() {
            format!("<img src=\"{}\"/>", escape_html(&banner))
        } else {
            String::new()
        },
    );

    let html = super::heroku_info::render(
        &custom.replacen("{}", &super::heroku_info::platform_custom_emoji(), 1),
        &data,
        &ctx.tr("info.missing_placeholder"),
    );

    if rich {
        crate::protection::guard().await;
        let input = InputMessage::new().rich_message(InputRichMessage::html(&html));
        response.inner.edit(input).await?;
        return Ok(());
    }

    if !banner.is_empty() && quote {
        crate::protection::guard().await;
        let input = InputMessage::new()
            .html(format!("{html}\n{}", escape_html(&banner)))
            .link_preview(true);
        response.inner.edit(input).await?;
        return Ok(());
    }

    if !banner.is_empty() {
        crate::protection::guard().await;
        let input = InputMessage::new()
            .html(html.clone())
            .photo_url(banner.clone())
            .invert_media(invert);
        if response.inner.edit(input).await.is_ok() {
            return Ok(());
        }
    }

    response.edit_html(html).await?;
    Ok(())
}

async fn logs_command(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
    let force = args.flag("f") || args.flag("force");
    let mut modules = Vec::new();
    let mut level: Option<i32> = None;

    for token in args.raw.split_whitespace() {
        if token == "-f" || token == "--force" {
            continue;
        }
        if let Ok(value) = token.parse::<i32>() {
            level = Some(value);
            continue;
        }
        let upper = token.to_uppercase();
        if let Some(value) = match upper.as_str() {
            "CRITICAL" => Some(50),
            "ERROR" => Some(40),
            "WARNING" | "WARN" => Some(30),
            "INFO" => Some(20),
            "DEBUG" => Some(10),
            "ALL" | "NOTSET" => Some(0),
            _ => None,
        } {
            level = Some(value);
            continue;
        }
        // Python resolves every non-level argument to a loaded module and
        // aborts with `bad_module` otherwise.
        let Some(module) = ctx.modules.get(token) else {
            message
                .reply_html(ctx.tf("test.bad_module", &[&escape_html(token)]))
                .await?;
            return Ok(());
        };
        modules.push(module.name().to_owned());
    }

    let Some(level) = level else {
        let user_id = message.sender_id().unwrap_or(ctx.me_id);
        let (html, buttons) = level_card(ctx, &modules);
        if ctx
            .inline
            .send_to_user(ctx, user_id, html, buttons)
            .await
            .is_ok()
        {
            return Ok(());
        }
        message.reply_html(ctx.tr("test.set_loglevel")).await?;
        return Ok(());
    };

    if level < 30 && !force {
        let user_id = message.sender_id().unwrap_or(ctx.me_id);
        let (html, buttons) = confidential_card(ctx, level, &modules);
        if ctx
            .inline
            .send_to_user(ctx, user_id, html, buttons)
            .await
            .is_ok()
        {
            return Ok(());
        }
        message
            .reply_html(ctx.tf("test.confidential_text", &[&level_name(level)]))
            .await?;
        return Ok(());
    }

    deliver_logs(ctx, message, level, &modules).await
}

fn level_name(level: i32) -> String {
    match level {
        50 => "CRITICAL",
        40 => "ERROR",
        30 => "WARNING",
        20 => "INFO",
        10 => "DEBUG",
        _ => "ALL",
    }
    .to_owned()
}

/// Map a module display name to its tracing target (source module).
fn log_target(display: &str) -> String {
    let target = match display {
        "APIRatelimiterMod" => "api_ratelimiter",
        "Evaluator" => "eval",
        "Help" => "help",
        "HerokuBackupMod" => "heroku_backup",
        "HerokuConfigMod" => "heroku_config",
        "HerokuInfoMod" => "heroku_info",
        "HerokuSecurityMod" => "heroku_security",
        "HerokuSettingsMod" => "heroku_settings",
        "InlineStuff" => "inline",
        "LoaderMod" => "loader",
        "LoaderRestrictor" => "loader_restrictor",
        "Quickstart" => "quickstart",
        "CoreMod" => "settings",
        "TerminalMod" => "terminal",
        "TestMod" => "test",
        "Translator" => "translate",
        "Translations" => "translations",
        "UpdaterMod" => "updater",
        "HerokuAccounts" => "web",
        other => other,
    };
    target.to_lowercase()
}

fn log_targets(modules: &[String]) -> Vec<String> {
    modules.iter().map(|module| log_target(module)).collect()
}

fn level_card(ctx: &Arc<Ctx>, modules: &[String]) -> (String, Vec<Vec<Button>>) {
    let suffix = if modules.is_empty() {
        String::new()
    } else {
        format!(":{}", modules.join(","))
    };
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for (name, level) in LEVELS {
        row.push(Button::data(
            (*name).to_owned(),
            format!("heroku:logs:level:{level}{suffix}").into_bytes(),
        ));
        if row.len() == 2 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![Button::data(
        ctx.tr("test.cancel"),
        b"heroku:close".to_vec(),
    )]);
    let text = if modules.is_empty() {
        ctx.tr("test.choose_loglevel")
    } else {
        ctx.tf("test.choose_loglevel_of", &[&modules.join(", ")])
    };
    (text, rows)
}

fn confidential_card(ctx: &Arc<Ctx>, level: i32, modules: &[String]) -> (String, Vec<Vec<Button>>) {
    let suffix = if modules.is_empty() {
        String::new()
    } else {
        format!(":{}", modules.join(","))
    };
    (
        ctx.tf("test.confidential", &[&level_name(level)]),
        vec![
            vec![Button::data(
                ctx.tr("test.send_anyway"),
                format!("heroku:logs:force:{level}{suffix}").into_bytes(),
            )],
            vec![Button::data(
                ctx.tr("test.cancel"),
                b"heroku:close".to_vec(),
            )],
        ],
    )
}

async fn deliver_logs(ctx: &Arc<Ctx>, message: &Msg, level: i32, modules: &[String]) -> Result<()> {
    let targets = log_targets(modules);
    let lines = ctx.log_buffer.select(level, &targets);
    let named = level_name(level);
    if lines.is_empty() {
        message
            .reply_html(ctx.tf("test.no_logs", &[&named]))
            .await?;
        return Ok(());
    }

    let path = ctx.data_root.join("heroku-logs.txt");
    std::fs::write(&path, lines.join("\n"))?;
    let caption = ctx.tf("test.logs_caption", &[&named]);
    let result = message.reply_file(&ctx.client, &path, caption).await;
    let _ = std::fs::remove_file(&path);
    result?;
    Ok(())
}

async fn deliver_logs_call(ctx: &Arc<Ctx>, call: &CallbackQuery, level: i32, modules: &[String]) {
    let targets = log_targets(modules);
    let lines = ctx.log_buffer.select(level, &targets);
    let named = level_name(level);
    if lines.is_empty() {
        let _ = call
            .answer()
            .alert(ctx.tf("test.no_logs", &[&named]))
            .send()
            .await;
        return;
    }

    let path = ctx.data_root.join("heroku-logs.txt");
    if std::fs::write(&path, lines.join("\n")).is_err() {
        let _ = call.answer().alert("failed to write logs").send().await;
        return;
    }
    let caption = ctx.tf("test.logs_caption", &[&named]);
    let result = async {
        let peer = call.peer_ref().await.ok().flatten()?;
        crate::protection::guard().await;
        let uploaded = ctx.client.upload_file(&path).await.ok()?;
        let message = InputMessage::new().html(caption).document(uploaded);
        ctx.client.send_message(peer, message).await.ok()
    }
    .await;
    let _ = std::fs::remove_file(&path);
    let _ = call
        .answer()
        .text(if result.is_some() {
            ctx.tr("test.logs_sent")
        } else {
            ctx.tf("test.no_logs", &[&named])
        })
        .send()
        .await;
}

/// Logs callbacks: level picker, confidential confirmation and "send anyway".
pub(crate) async fn handle_inline_callback(
    ctx: &Arc<Ctx>,
    call: &CallbackQuery,
    data: &str,
) -> Result<bool> {
    if let Some(raw) = data.strip_prefix("heroku:logs:level:") {
        let (level_raw, modules) = match raw.split_once(':') {
            Some((level, modules)) => (
                level,
                modules.split(',').map(str::to_owned).collect::<Vec<_>>(),
            ),
            None => (raw, Vec::new()),
        };
        let Some(level) = level_raw.parse::<i32>().ok() else {
            let _ = call.answer().send().await;
            return Ok(true);
        };
        if level < 30 {
            let (html, buttons) = confidential_card(ctx, level, &modules);
            let message = InputMessage::new()
                .html(html)
                .reply_markup(ReplyMarkup::from_buttons(&buttons));
            let _ = call.answer().edit(message).await;
        } else {
            deliver_logs_call(ctx, call, level, &modules).await;
        }
        return Ok(true);
    }

    if let Some(raw) = data.strip_prefix("heroku:logs:force:") {
        let (level, modules) = match raw.split_once(':') {
            Some((level, modules)) => (
                level.parse::<i32>().unwrap_or(20),
                modules.split(',').map(str::to_owned).collect(),
            ),
            None => (raw.parse::<i32>().unwrap_or(20), Vec::new()),
        };
        deliver_logs_call(ctx, call, level, &modules).await;
        return Ok(true);
    }

    Ok(false)
}

inventory::submit! {
    ModuleFactory(|| Box::new(TestMod))
}
