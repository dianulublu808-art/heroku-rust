// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.info` — userbot information with configurable templates, banner media
//! and real system metrics (CPU, RAM, OS, git state).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use grammers_client::message::{InputMessage, InputRichMessage};

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

const NS: &str = "HerokuInfoMod";
const DEFAULT_BANNER: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/heroku_info.jpg";

pub struct HerokuInfoMod;

#[async_trait]
impl Module for HerokuInfoMod {
    fn name(&self) -> &'static str {
        "HerokuInfoMod"
    }

    fn description(&self) -> &'static str {
        "Show userbot information"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("info", "Show userbot information"),
            CommandDef::new("ubinfo", "Explain what a userbot is"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "custom_message",
                "Custom message for info. Supports {me}, {version}, {build}, {prefix}, \
                 {platform}, {platform_emoji}, {upd}, {uptime}, {cpu_usage}, {ram_usage}, \
                 {branch}, {hostname}, {user}, {os}, {kernel}, {cpu}, {ping}, {htl_ver}, \
                 {git_status}, {img}",
                ConfigDefault::Text(""),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "banner_url",
                "URL to the image banner shown above .info",
                ConfigDefault::Text(DEFAULT_BANNER),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "ping_emoji",
                "Emoji shown while the ping is being measured",
                ConfigDefault::Text("🪐"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "quote_media",
                "Show the banner as a link preview instead of a photo",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "invert_media",
                "Show the banner below the text",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "rich_mode",
                "Render .info as a Telegram rich message",
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
        _args: &Args,
    ) -> Result<()> {
        if command == "ubinfo" {
            message.reply_html(ctx.tr("info.desc")).await?;
            return Ok(());
        }

        let start = Instant::now();
        let custom = text_config(ctx, "custom_message", "");
        let banner = text_config(ctx, "banner_url", DEFAULT_BANNER);
        let rich_mode = bool_config(ctx, "rich_mode", false);
        let invert = bool_config(ctx, "invert_media", false);
        let quote_media = bool_config(ctx, "quote_media", false);
        let ping_emoji = text_config(ctx, "ping_emoji", "🪐");

        let mut data = placeholders(ctx, &banner).await;
        data.insert("ping".to_owned(), "0.000".to_owned());

        let template = if custom.is_empty() {
            let key = if rich_mode {
                "info.rich_message"
            } else {
                "info.message"
            };
            ctx.tr(key)
        } else {
            custom.clone()
        };
        let template = template.replacen("{}", &platform_custom_emoji(), 1);

        // Report the ping as the time spent gathering metrics and git state.
        data.insert(
            "ping".to_owned(),
            format!("{:.3}", start.elapsed().as_secs_f64() * 1000.0),
        );
        let missing = ctx.tr("info.missing_placeholder");
        let html = render(&template, &data, &missing);

        if !custom.is_empty() && custom.contains("{ping}") {
            let placeholder = message.reply_html(&ping_emoji).await?;
            placeholder.edit_html(html).await?;
            return Ok(());
        }

        if rich_mode {
            crate::protection::guard().await;
            let input = InputMessage::new().rich_message(InputRichMessage::html(html));
            message.inner.reply(input).await?;
            return Ok(());
        }

        if quote_media && !banner.is_empty() {
            crate::protection::guard().await;
            let input = InputMessage::new()
                .html(format!("{html}\n{}", escape_html(&banner)))
                .link_preview(true);
            message.inner.reply(input).await?;
            return Ok(());
        }

        if !banner.is_empty() {
            crate::protection::guard().await;
            let input = InputMessage::new()
                .html(html.clone())
                .photo_url(banner.clone())
                .invert_media(invert);
            match message.inner.reply(input).await {
                Ok(_) => return Ok(()),
                Err(err) => {
                    tracing::warn!("Failed to send the info banner: {err}");
                    message
                        .reply_html(ctx.tf("info.no_banner", &[&escape_html(&banner)]))
                        .await?;
                }
            }
        }

        message.reply_html(html).await?;
        Ok(())
    }
}

fn text_config(ctx: &Ctx, key: &str, default: &str) -> String {
    ctx.db.get_str(NS, key, default)
}

fn bool_config(ctx: &Ctx, key: &str, default: bool) -> bool {
    ctx.db.get_bool(NS, key, default)
}

async fn placeholders(ctx: &Arc<Ctx>, banner: &str) -> HashMap<String, String> {
    let mut data = HashMap::new();

    let owner = ctx.security.owners().first().copied().unwrap_or(ctx.me_id);
    data.insert(
        "me".to_owned(),
        format!(
            "<b><a href=\"tg://user?id={owner}\">{}</a></b>",
            escape_html(&ctx.me_name)
        ),
    );
    data.insert("version".to_owned(), crate::config::VERSION.to_owned());
    data.insert("build".to_owned(), commit_url().await);
    let prefix = ctx
        .prefixes(ctx.me_id)
        .into_iter()
        .next()
        .unwrap_or_else(|| ".".to_owned());
    data.insert("prefix".to_owned(), format!("«{}»", escape_html(&prefix)));
    data.insert("platform".to_owned(), platform_name());
    data.insert("platform_emoji".to_owned(), platform_emoji());
    data.insert("upd".to_owned(), update_status(ctx).await);
    data.insert("uptime".to_owned(), ctx.uptime());
    data.insert("cpu_usage".to_owned(), cpu_usage().await);
    data.insert("ram_usage".to_owned(), ram_usage());
    data.insert("branch".to_owned(), branch().await);
    data.insert("hostname".to_owned(), hostname());
    data.insert("user".to_owned(), current_user());
    data.insert("os".to_owned(), os_name());
    data.insert("kernel".to_owned(), kernel());
    data.insert("cpu".to_owned(), cpu_info());
    data.insert("htl_ver".to_owned(), "grammers 0.12".to_owned());
    data.insert("git_status".to_owned(), git_status().await);
    data.insert(
        "img".to_owned(),
        if banner.is_empty() {
            String::new()
        } else {
            format!("<img src=\"{}\"/>", escape_html(banner))
        },
    );
    data
}

/// Replace `{name}` placeholders, keeping unknown ones visible as an error so
/// typos in a custom message are easy to spot (same as Python).
fn render(template: &str, data: &HashMap<String, String>, missing: &str) -> String {
    let mut out = String::with_capacity(template.len());
    let chars: Vec<char> = template.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '{' {
            out.push(chars[index]);
            index += 1;
            continue;
        }
        let Some(end) = chars[index + 1..].iter().position(|ch| *ch == '}') else {
            out.push(chars[index]);
            index += 1;
            continue;
        };
        let name: String = chars[index + 1..index + 1 + end].iter().collect();
        if !name.is_empty() && name.chars().all(|ch| ch.is_alphanumeric() || ch == '_') {
            match data.get(&name) {
                Some(value) => out.push_str(value),
                None => out.push_str(
                    &missing.replace("{placeholder}", &escape_html(&format!("{{{name}}}"))),
                ),
            }
            index += end + 2;
        } else {
            out.push(chars[index]);
            index += 1;
        }
    }
    out
}

fn platform_name() -> String {
    if let Ok(model) = std::fs::read_to_string("/proc/device-tree/model") {
        let model = model.trim().to_owned();
        if model.contains("Orange") || model.contains("Raspberry") {
            return model;
        }
    }
    if std::env::var("WSL_DISTRO_NAME").is_ok() {
        "WSL".to_owned()
    } else if std::env::var("HIKKAHOST").is_ok() {
        "HikkaHost".to_owned()
    } else if std::env::var("DOCKER").is_ok() {
        "Docker".to_owned()
    } else if std::env::var("PATH")
        .map(|path| path.contains("com.apple"))
        .unwrap_or(false)
    {
        "MacOS".to_owned()
    } else if std::env::var("USER")
        .map(|user| user.to_lowercase().contains("userland"))
        .unwrap_or(false)
    {
        "UserLand".to_owned()
    } else if std::env::consts::OS == "android" {
        "Android".to_owned()
    } else {
        "VDS".to_owned()
    }
}

fn platform_emoji() -> String {
    if let Ok(model) = std::fs::read_to_string("/proc/device-tree/model") {
        if model.contains("Orange") {
            return "🍊".to_owned();
        }
        if model.contains("Raspberry") {
            return "🍇".to_owned();
        }
    }
    match platform_name().as_str() {
        "WSL" => "🍀",
        "Windows" => "💻",
        "MacOS" => "🍏",
        "UserLand" => "🐧",
        "HikkaHost" => "🌼",
        "Docker" => "🐳",
        _ => "💎",
    }
    .to_owned()
}

fn platform_custom_emoji() -> String {
    let id = match platform_name().as_str() {
        "HikkaHost" => "5395745114494624362",
        "UserLand" => "5458877818031077824",
        "Docker" => "5352678227582152630",
        _ => "5393588431026674882",
    };
    format!(
        "<tg-emoji emoji-id={id}>🪐</tg-emoji>\
         <tg-emoji emoji-id=5352934134618549768>🪐</tg-emoji>\
         <tg-emoji emoji-id=5352663371290271790>🪐</tg-emoji>\
         <tg-emoji emoji-id=5350822883314655367>🪐</tg-emoji>"
    )
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|name| name.trim().to_owned())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| std::env::consts::OS.to_owned())
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "user".to_owned())
}

fn os_name() -> String {
    if let Ok(raw) = std::fs::read_to_string("/etc/os-release") {
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                return value.trim().trim_matches('"').to_owned();
            }
        }
    }
    std::env::consts::OS.to_owned()
}

fn kernel() -> String {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| release.trim().to_owned())
        .unwrap_or_else(|_| std::env::consts::ARCH.to_owned())
}

fn cpu_info() -> String {
    let cores = std::fs::read_to_string("/proc/cpuinfo")
        .map(|raw| {
            raw.lines()
                .filter(|line| line.starts_with("processor"))
                .count()
        })
        .unwrap_or(0);
    if cores == 0 {
        std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1)
            .to_string()
    } else {
        format!("{cores} core(s)")
    }
}

fn cpu_times() -> Option<(u64, u64)> {
    let raw = std::fs::read_to_string("/proc/stat").ok()?;
    let line = raw.lines().next()?;
    let values: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse().ok())
        .collect();
    if values.len() < 4 {
        return None;
    }
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total: u64 = values.iter().sum();
    Some((idle, total))
}

async fn cpu_usage() -> String {
    let Some((idle_a, total_a)) = cpu_times() else {
        return String::new();
    };
    tokio::time::sleep(Duration::from_millis(80)).await;
    let Some((idle_b, total_b)) = cpu_times() else {
        return String::new();
    };
    let total = total_b.saturating_sub(total_a);
    let idle = idle_b.saturating_sub(idle_a);
    if total == 0 {
        return "0.00%".to_owned();
    }
    format!("{:.2}%", (total - idle) as f64 / total as f64 * 100.0)
}

fn ram_usage() -> String {
    let Ok(raw) = std::fs::read_to_string("/proc/self/statm") else {
        return "—".to_owned();
    };
    let pages: u64 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let page_size = 4096_u64;
    format!(
        "{:.1} MB",
        pages as f64 * page_size as f64 / (1024.0 * 1024.0)
    )
}

async fn branch() -> String {
    let Some(root) = crate::modules::updater::repo_root() else {
        return "Unknown".to_owned();
    };
    crate::modules::updater::git(&root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "Unknown".to_owned())
}

async fn git_status() -> String {
    let Some(root) = crate::modules::updater::repo_root() else {
        return "Not a Git repo".to_owned();
    };
    match crate::modules::updater::git(&root, &["status", "--porcelain"]).await {
        Ok(output) if output.is_empty() => "Clean".to_owned(),
        Ok(output) => {
            let count = output.lines().count();
            let word = if count == 1 { "file" } else { "files" };
            format!("{count} {word} modified")
        }
        Err(_) => "Unknown".to_owned(),
    }
}

async fn commit_url() -> String {
    let Some(root) = crate::modules::updater::repo_root() else {
        return "Unknown".to_owned();
    };
    let Ok(hash) = crate::modules::updater::git(&root, &["rev-parse", "--short", "HEAD"]).await
    else {
        return "Unknown".to_owned();
    };
    format!(
        "<a href=\"https://github.com/dianulublu808-art/heroku-rust/commit/{hash}\">#{hash}</a>"
    )
}

async fn update_status(ctx: &Arc<Ctx>) -> String {
    let Some(root) = crate::modules::updater::repo_root() else {
        return String::new();
    };
    let Ok(branch) =
        crate::modules::updater::git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).await
    else {
        return String::new();
    };
    let upstream = format!("origin/{branch}");
    let Ok(behind) = crate::modules::updater::git(
        &root,
        &["rev-list", "--count", &format!("HEAD..{upstream}")],
    )
    .await
    else {
        return String::new();
    };
    if behind.trim() == "0" {
        ctx.tr("info.up_to_date")
    } else {
        let prefix = ctx
            .prefixes(ctx.me_id)
            .into_iter()
            .next()
            .unwrap_or_else(|| ".".to_owned());
        ctx.tf("info.update_required", &[&escape_html(&prefix)])
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuInfoMod))
}
