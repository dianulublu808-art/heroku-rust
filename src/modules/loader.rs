// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Module loader: repositories, downloads and local module storage.
//!
//! Rust modules are compiled into the binary. The loader implements the real
//! repository workflow used by Heroku: it fetches module indexes (`full.txt`),
//! downloads module files into `modules/` and keeps them in sync so they can be
//! reviewed, backed up and restored.
//!
//! `.rs` files are additionally mirrored into the repository `modules/`
//! directory, rebuilt with `cargo build --release` and loaded after a restart.
//! Python modules are stored and backed up but are not executed: they must be
//! ported to Rust.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use grammers_client::message::{Button, InputMessage, ReplyMarkup};
use grammers_client::update::CallbackQuery;
use rand::Rng;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub const NS: &str = "LoaderMod";
pub const DEFAULT_REPO: &str = "https://raw.githubusercontent.com/coddrago/modules/main";

const HTTP_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
const GALLERY_PAGE: usize = 8;

/// Repository index cached for the inline module gallery.
static MODULE_CACHE: LazyLock<Mutex<Vec<(String, String)>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

pub struct LoaderMod;

#[async_trait]
impl Module for LoaderMod {
    fn name(&self) -> &'static str {
        "LoaderMod"
    }

    fn description(&self) -> &'static str {
        "Load modules from repositories"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("ml", "Search repositories and send a module as a file")
                .usage("<module>"),
            CommandDef::new("dlmod", "Install a module from a repository")
                .usage("<module>")
                .aliases(&["dlm"]),
            CommandDef::new("loadmod", "Download a module from a direct URL")
                .usage("<url>")
                .aliases(&["lm"]),
            CommandDef::new("unloadmod", "Remove a downloaded module file")
                .usage("<module>")
                .aliases(&["ulm"]),
            CommandDef::new("reloadmod", "Re-download a module from its repository")
                .usage("<module>"),
            CommandDef::new(
                "loadall",
                "Download every module from the primary repository",
            ),
            CommandDef::new("unloadall", "Remove every downloaded module file"),
            CommandDef::new("listmods", "List built-in and downloaded modules"),
            CommandDef::new("modinfo", "Show information about a module").usage("<module>"),
            CommandDef::new("addrepo", "Add a module repository").usage("<url>"),
            CommandDef::new("delrepo", "Remove a module repository").usage("<url>"),
            CommandDef::new("dlmall", "Download all modules from a repository")
                .usage("[repository]"),
            CommandDef::new("clearmodules", "Delete all downloaded modules").usage("[--force]"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "MODULES_REPO",
                "URL of the primary module repository",
                ConfigDefault::Text(DEFAULT_REPO),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "ADDITIONAL_REPOS",
                "Additional repositories to search for modules",
                ConfigDefault::List(&[]),
                ConfigKind::List,
            ),
            ConfigSpec::new(
                "basic_auth",
                "Basic authentication for the module repository (user:password)",
                ConfigDefault::Text(""),
                ConfigKind::Hidden,
            ),
            ConfigSpec::new(
                "share_link",
                "Include a link to the module source after loading it",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "command_emoji",
                "Emoji used for commands in module galleries",
                ConfigDefault::Text("▫️"),
                ConfigKind::Text,
            ),
            ConfigSpec::new(
                "rich_mode",
                "Render module cards as Telegram rich messages",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "show_banner",
                "Show the module banner in load cards",
                ConfigDefault::Bool(true),
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
            "addrepo" => self.add_repo(ctx, message, args).await,
            "delrepo" => self.del_repo(ctx, message, args).await,
            "dlmall" => {
                if crate::modules::loader_restrictor::blocked(ctx, message).await {
                    return Ok(());
                }
                self.download_all(ctx, message, args).await
            }
            "ml" => self.send_module(ctx, message, args).await,
            "dlmod" if args.get(0).is_none() => {
                if crate::modules::loader_restrictor::blocked(ctx, message).await {
                    return Ok(());
                }
                self.gallery(ctx, message, 0).await
            }
            "dlmod" | "reloadmod" => {
                if crate::modules::loader_restrictor::blocked(ctx, message).await {
                    return Ok(());
                }
                self.install(ctx, message, args).await
            }
            "loadmod" => {
                if crate::modules::loader_restrictor::blocked(ctx, message).await {
                    return Ok(());
                }
                self.load_url(ctx, message, args).await
            }
            "unloadmod" => self.unload(ctx, message, args).await,
            "loadall" => self.load_all(ctx, message).await,
            "unloadall" => self.unload_all(ctx, message).await,
            "clearmodules" => self.clear(ctx, message, args).await,
            "listmods" => self.list(ctx, message).await,
            "modinfo" => self.info(ctx, message, args).await,
            _ => Ok(()),
        }
    }
}

impl LoaderMod {
    async fn add_repo(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(raw) = args.get(0) else {
            message.reply_html(ctx.tr("loader.no_repo")).await?;
            return Ok(());
        };
        let Some(repo) = normalize_repo(raw) else {
            message.reply_html(ctx.tr("loader.no_repo")).await?;
            return Ok(());
        };

        if repos(ctx).iter().any(|known| known == &repo) {
            message
                .reply_html(ctx.tf("loader.repo_exists", &[&escape_html(&repo)]))
                .await?;
            return Ok(());
        }

        match fetch_modules(ctx, &repo).await {
            Ok(modules) if !modules.is_empty() => {
                let mut additional = additional_repos(ctx);
                additional.push(repo.clone());
                set_additional_repos(ctx, &additional);
                message
                    .reply_html(ctx.tf("loader.repo_added", &[&escape_html(&repo)]))
                    .await?;
            }
            _ => {
                message.reply_html(ctx.tr("loader.no_repo")).await?;
            }
        }
        Ok(())
    }

    async fn del_repo(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(raw) = args.get(0) else {
            message.reply_html(ctx.tr("loader.no_repo")).await?;
            return Ok(());
        };
        let Some(repo) = normalize_repo(raw) else {
            message.reply_html(ctx.tr("loader.no_repo")).await?;
            return Ok(());
        };

        let mut additional = additional_repos(ctx);
        let Some(position) = additional.iter().position(|known| known == &repo) else {
            message
                .reply_html(ctx.tf("loader.repo_not_exists", &[&escape_html(&repo)]))
                .await?;
            return Ok(());
        };
        additional.remove(position);
        set_additional_repos(ctx, &additional);
        message
            .reply_html(ctx.tf("loader.repo_deleted", &[&escape_html(&repo)]))
            .await?;
        Ok(())
    }

    async fn download_all(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let known = repos(ctx);
        if known.is_empty() {
            message.reply_html(ctx.tr("loader.repos_empty")).await?;
            return Ok(());
        }

        let Some(raw) = args.get(0) else {
            let mut lines = vec![ctx.tr("loader.choose_repo")];
            for (index, repo) in known.iter().enumerate() {
                lines.push(format!(
                    "▫️ <code>{}</code> — <code>.dlmall {}</code>",
                    escape_html(repo),
                    index + 1
                ));
            }
            let mut rows = Vec::new();
            for (index, repo) in known.iter().enumerate() {
                rows.push(vec![Button::data(
                    repo_host(repo),
                    format!("heroku:mod:repo:{index}").into_bytes(),
                )]);
            }
            rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
            if ctx
                .inline
                .send_to_user(ctx, ctx.me_id, lines.join("\n"), rows)
                .await
                .is_err()
            {
                message.reply_html(lines.join("\n")).await?;
            }
            return Ok(());
        };

        let repo = if let Ok(index) = raw.trim().parse::<usize>() {
            known.get(index.saturating_sub(1)).cloned()
        } else {
            normalize_repo(raw)
        };
        let Some(repo) = repo else {
            message.reply_html(ctx.tr("loader.no_repo")).await?;
            return Ok(());
        };

        message
            .reply_html(ctx.tf("loader.installing_all", &[&escape_html(&repo)]))
            .await?;

        let modules = fetch_modules(ctx, &repo).await?;
        if modules.is_empty() {
            message
                .reply_html(ctx.tr("loader.no_modules_in_repo"))
                .await?;
            return Ok(());
        }

        let mut installed = 0usize;
        let mut rust = false;
        let mut failed = Vec::new();
        for entry in modules {
            match install_entry(ctx, &repo, &entry).await {
                Ok(path) => {
                    installed += 1;
                    rust |= is_rust_module(&path);
                }
                Err(err) => failed.push(format!(
                    "▫️ <code>{}</code> — {}",
                    escape_html(&entry),
                    escape_html(&err.to_string())
                )),
            }
        }

        if failed.is_empty() {
            message
                .reply_html(ctx.tf("loader.installed_all", &[&installed.to_string()]))
                .await?;
        } else {
            let mut text = ctx.tf(
                "loader.installed_partial",
                &[&installed.to_string(), &failed.len().to_string()],
            );
            text.push('\n');
            text.push_str(&failed.join("\n"));
            message.reply_html(text).await?;
        }
        if rust {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn send_module(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("loader.args")).await?;
            return Ok(());
        };

        // `.ml` sends a module file. Downloaded modules already live on disk,
        // so prefer them over downloading the same file again.
        if let Some(path) = find_local_module(ctx, name) {
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("module.py")
                .to_owned();
            message
                .respond_file(
                    &ctx.client,
                    &path,
                    ctx.tf("loader.ml_ready", &[&escape_html(&file_name)]),
                )
                .await?;
            return Ok(());
        }

        message.reply_html(ctx.tr("loader.finding_module")).await?;
        let Some((repo, entry)) = find_module(ctx, name).await? else {
            message
                .reply_html(ctx.tr("loader.module_not_found"))
                .await?;
            return Ok(());
        };

        let path = install_entry(ctx, &repo, &entry).await?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("module.py")
            .to_owned();
        message
            .respond_file(
                &ctx.client,
                &path,
                ctx.tf("loader.ml_ready", &[&escape_html(&file_name)]),
            )
            .await?;
        Ok(())
    }

    async fn install(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("loader.args")).await?;
            return Ok(());
        };

        message.reply_html(ctx.tr("loader.finding_module")).await?;
        let Some((repo, entry)) = find_module(ctx, name).await? else {
            message
                .reply_html(ctx.tr("loader.module_not_found"))
                .await?;
            return Ok(());
        };

        let path = install_entry(ctx, &repo, &entry).await?;
        message
            .reply_html(ctx.tf(
                "loader.module_installed",
                &[&escape_html(&display_name(&path))],
            ))
            .await?;
        if is_rust_module(&path) {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn gallery(&self, ctx: &Arc<Ctx>, message: &Msg, page: usize) -> Result<()> {
        message.reply_html(ctx.tr("loader.finding_module")).await?;
        refresh_module_cache(ctx).await?;
        let (html, buttons) = module_gallery_card(ctx, page);
        match ctx
            .inline
            .send_to_user(ctx, ctx.me_id, html.clone(), buttons)
            .await
        {
            Ok(()) => {
                message.reply_html(ctx.tr("loader.gallery_sent")).await?;
            }
            Err(_) => {
                message.reply_html(html).await?;
            }
        }
        Ok(())
    }

    async fn load_url(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(raw) = args.get(0) else {
            message.reply_html(ctx.tr("loader.loadmod_usage")).await?;
            return Ok(());
        };
        if !raw.starts_with("http://") && !raw.starts_with("https://") {
            message.reply_html(ctx.tr("loader.loadmod_usage")).await?;
            return Ok(());
        }

        message.reply_html(ctx.tr("loader.loading_module")).await?;
        let name = raw
            .rsplit('/')
            .next()
            .filter(|part| has_known_extension(part))
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| format!("module-{:08x}.py", rand::thread_rng().gen::<u32>()));
        let path = download_to_modules(ctx, raw, &name).await?;
        message
            .reply_html(ctx.tf(
                "loader.module_installed",
                &[&escape_html(&display_name(&path))],
            ))
            .await?;
        if is_rust_module(&path) {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn unload(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("loader.args")).await?;
            return Ok(());
        };

        let dir = modules_dir(ctx);
        let target = sanitize_file_name(name);
        let path = dir.join(&target);
        let victim = if path.exists() {
            Some(path)
        } else {
            find_local_module(ctx, name)
        };
        let Some(victim) = victim else {
            message
                .reply_html(ctx.tr("loader.module_not_found"))
                .await?;
            return Ok(());
        };

        let rust = is_rust_module(&victim);
        std::fs::remove_file(&victim)
            .with_context(|| format!("failed to remove {}", victim.display()))?;
        if rust {
            remove_repo_module(&victim);
        }
        message
            .reply_html(ctx.tf(
                "loader.module_unloaded",
                &[&escape_html(&display_name(&victim))],
            ))
            .await?;
        if rust {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn load_all(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let repo = primary_repo(ctx);
        let modules = fetch_modules(ctx, &repo).await?;
        if modules.is_empty() {
            message
                .reply_html(ctx.tr("loader.no_modules_in_repo"))
                .await?;
            return Ok(());
        }
        let mut installed = 0usize;
        let mut rust = false;
        for entry in modules {
            if let Ok(path) = install_entry(ctx, &repo, &entry).await {
                installed += 1;
                rust |= is_rust_module(&path);
            }
        }
        message
            .reply_html(ctx.tf("loader.installed_all", &[&installed.to_string()]))
            .await?;
        if rust {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn unload_all(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let had_rust = has_rust_modules(ctx);
        let removed = clear_modules(ctx)?;
        message
            .reply_html(ctx.tf("loader.modules_unloaded", &[&removed.to_string()]))
            .await?;
        if had_rust {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn clear(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let confirmed = args
            .get(0)
            .map(|raw| matches!(raw.to_lowercase().as_str(), "--force" | "yes" | "confirm"))
            .unwrap_or(false);
        if !confirmed {
            message
                .reply_html(ctx.tr("loader.confirm_clearmodules"))
                .await?;
            return Ok(());
        }
        let had_rust = has_rust_modules(ctx);
        let removed = clear_modules(ctx)?;
        message
            .reply_html(ctx.tf("loader.modules_unloaded", &[&removed.to_string()]))
            .await?;
        if had_rust {
            finish_rust_change(ctx, Some(message), true).await?;
        }
        Ok(())
    }

    async fn list(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let mut lines = vec![ctx.tr("loader.list_title")];
        for module in ctx.modules.all() {
            lines.push(format!(
                "▪️ <code>{}</code> — <i>{}</i>",
                escape_html(module.name()),
                escape_html(module.description())
            ));
        }

        let downloaded = list_local_modules(ctx);
        if !downloaded.is_empty() {
            lines.push(ctx.tr("loader.list_downloaded"));
            for path in downloaded {
                let size = std::fs::metadata(&path)
                    .map(|meta| meta.len())
                    .unwrap_or_default();
                lines.push(format!(
                    "📦 <code>{}</code> — <i>{} B</i>",
                    escape_html(&display_name(&path)),
                    size
                ));
            }
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn info(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(name) = args.get(0) else {
            message.reply_html(ctx.tr("common.no_args")).await?;
            return Ok(());
        };

        if let Some(module) = ctx.modules.get(name) {
            message
                .reply_html(format!(
                    "🧩 <b>{}</b>\n<i>{}</i>\n\n▫️ {} <code>{}</code>\n▫️ {} <code>{}</code>",
                    escape_html(module.name()),
                    escape_html(module.description()),
                    ctx.tr("help.commands"),
                    module.commands().len(),
                    ctx.tr("help.author"),
                    escape_html(module.author())
                ))
                .await?;
            return Ok(());
        }

        if let Some(path) = find_local_module(ctx, name) {
            let size = std::fs::metadata(&path)
                .map(|meta| meta.len())
                .unwrap_or_default();
            message
                .reply_html(ctx.tf(
                    "loader.info_downloaded",
                    &[&escape_html(&display_name(&path)), &size.to_string()],
                ))
                .await?;
            return Ok(());
        }

        message
            .reply_html(ctx.tr("loader.module_not_found"))
            .await?;
        Ok(())
    }
}

pub struct LoaderRestrictor;

#[async_trait]
impl Module for LoaderRestrictor {
    fn name(&self) -> &'static str {
        "LoaderRestrictor"
    }

    fn description(&self) -> &'static str {
        "Control which downloaded modules are allowed"
    }

    async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
        crate::modules::loader_restrictor::on_load(ctx).await
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("allowmod", "Allow a module to be installed").usage("<module>"),
            CommandDef::new("denymod", "Deny a module from being installed").usage("<module>"),
            CommandDef::new("allowedmods", "List allowed and denied modules"),
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
            "allowmod" => {
                let Some(name) = args.get(0) else {
                    message.reply_html(ctx.tr("common.no_args")).await?;
                    return Ok(());
                };
                let name = sanitize_stem(name);
                let mut allowed = ctx.db.get_string_list("LoaderRestrictor", "allowed");
                let mut denied = ctx.db.get_string_list("LoaderRestrictor", "denied");
                denied.retain(|item| item != &name);
                if !allowed.contains(&name) {
                    allowed.push(name.clone());
                }
                ctx.db.set("LoaderRestrictor", "allowed", allowed);
                ctx.db.set("LoaderRestrictor", "denied", denied);
                message
                    .reply_html(ctx.tf("loader.allow_added", &[&escape_html(&name)]))
                    .await?;
            }
            "denymod" => {
                let Some(name) = args.get(0) else {
                    message.reply_html(ctx.tr("common.no_args")).await?;
                    return Ok(());
                };
                let name = sanitize_stem(name);
                let mut allowed = ctx.db.get_string_list("LoaderRestrictor", "allowed");
                let mut denied = ctx.db.get_string_list("LoaderRestrictor", "denied");
                allowed.retain(|item| item != &name);
                if !denied.contains(&name) {
                    denied.push(name.clone());
                }
                ctx.db.set("LoaderRestrictor", "allowed", allowed);
                ctx.db.set("LoaderRestrictor", "denied", denied);
                message
                    .reply_html(ctx.tf("loader.deny_added", &[&escape_html(&name)]))
                    .await?;
            }
            _ => {
                let allowed = ctx.db.get_string_list("LoaderRestrictor", "allowed");
                let denied = ctx.db.get_string_list("LoaderRestrictor", "denied");
                let mut lines = vec![ctx.tr("loader.restrictor_title")];
                lines.push(format!(
                    "✅ <b>{}</b>: {}",
                    ctx.tr("loader.allowed"),
                    if allowed.is_empty() {
                        "—".to_owned()
                    } else {
                        allowed
                            .iter()
                            .map(|name| format!("<code>{}</code>", escape_html(name)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
                lines.push(format!(
                    "⛔️ <b>{}</b>: {}",
                    ctx.tr("loader.denied"),
                    if denied.is_empty() {
                        "—".to_owned()
                    } else {
                        denied
                            .iter()
                            .map(|name| format!("<code>{}</code>", escape_html(name)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
                message.reply_html(lines.join("\n")).await?;
            }
        }
        Ok(())
    }
}

pub fn modules_dir(ctx: &Arc<Ctx>) -> PathBuf {
    ctx.data_root.join("modules")
}

/// List files that have been downloaded into the module directory.
pub fn list_local_modules(ctx: &Arc<Ctx>) -> Vec<PathBuf> {
    let dir = modules_dir(ctx);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| matches!(ext, "py" | "rhai" | "rs"))
                .unwrap_or(false)
        })
        .collect();
    out.sort();
    out
}

async fn refresh_module_cache(ctx: &Arc<Ctx>) -> Result<()> {
    let mut fetched = Vec::new();
    let mut errors = Vec::new();
    for repo in repos(ctx) {
        match fetch_modules(ctx, &repo).await {
            Ok(entries) => {
                for entry in entries {
                    fetched.push((repo.clone(), entry));
                }
            }
            Err(err) => errors.push(format!("{repo}: {err}")),
        }
    }

    if fetched.is_empty() {
        if errors.is_empty() {
            bail!("{}", ctx.tr("loader.repos_empty"));
        }
        bail!(
            "{}",
            ctx.tf(
                "loader.repo_unavailable",
                &[&escape_html(&errors.join("; "))]
            )
        );
    }

    fetched.sort_by(|left, right| {
        module_stem(&left.1)
            .to_lowercase()
            .cmp(&module_stem(&right.1).to_lowercase())
    });
    fetched
        .dedup_by(|left, right| module_stem(&left.1).eq_ignore_ascii_case(&module_stem(&right.1)));

    *MODULE_CACHE
        .lock()
        .map_err(|_| anyhow::anyhow!("module cache lock poisoned"))? = fetched;
    Ok(())
}

fn module_gallery_card(ctx: &Arc<Ctx>, page: usize) -> (String, Vec<Vec<Button>>) {
    let cache = MODULE_CACHE
        .lock()
        .map(|cache| cache.clone())
        .unwrap_or_default();
    let pages = cache.len().div_ceil(GALLERY_PAGE).max(1);
    let page = page.min(pages - 1);
    let start = page * GALLERY_PAGE;
    let slice = &cache[start..cache.len().min(start + GALLERY_PAGE)];

    let mut lines = vec![
        ctx.tr("loader.gallery_title"),
        ctx.tf(
            "loader.gallery_page",
            &[
                &(page + 1).to_string(),
                &pages.to_string(),
                &cache.len().to_string(),
            ],
        ),
    ];
    let mut rows = Vec::new();

    for (offset, (repo, entry)) in slice.iter().enumerate() {
        let index = start + offset;
        let stem = module_stem(entry);
        let installed = modules_dir(ctx).join(entry_name(entry)).exists();
        lines.push(format!(
            "{} <code>{}</code>\n   <i>{}</i>",
            if installed { "✅" } else { "▫️" },
            escape_html(&stem),
            escape_html(&repo_host(repo))
        ));
        rows.push(vec![Button::data(
            format!("{} {stem}", if installed { "♻️" } else { "⬇️" }),
            format!("heroku:mod:in:{index}").into_bytes(),
        )]);
    }

    if cache.is_empty() {
        lines.push(ctx.tr("loader.gallery_empty").to_owned());
    }

    if pages > 1 {
        let mut row = Vec::new();
        if page > 0 {
            row.push(Button::data(
                "⬅️",
                format!("heroku:mod:list:{}", page - 1).into_bytes(),
            ));
        }
        if page + 1 < pages {
            row.push(Button::data(
                "➡️",
                format!("heroku:mod:list:{}", page + 1).into_bytes(),
            ));
        }
        if !row.is_empty() {
            rows.push(row);
        }
    }

    rows.push(vec![
        Button::data(ctx.tr("loader.gallery_all"), b"heroku:mod:all".to_vec()),
        Button::data(ctx.tr("loader.gallery_clear"), b"heroku:mod:clear".to_vec()),
    ]);
    rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    (lines.join("\n"), rows)
}

async fn show_gallery_call(ctx: &Arc<Ctx>, call: &CallbackQuery, page: usize) {
    let (html, buttons) = module_gallery_card(ctx, page);
    let _ = call
        .answer()
        .edit(
            InputMessage::new()
                .html(html)
                .reply_markup(ReplyMarkup::from_buttons(&buttons)),
        )
        .await;
}

/// Callback router for the inline module gallery. Returns `true` when the data
/// belonged to the loader, so the caller can stop dispatching it.
pub(crate) async fn handle_inline_callback(
    ctx: &Arc<Ctx>,
    call: &CallbackQuery,
    data: &str,
) -> Result<bool> {
    let installs = data.starts_with("heroku:mod:in:")
        || data == "heroku:mod:all"
        || data.starts_with("heroku:mod:repo:");
    if installs {
        if let Some(html) = crate::modules::loader_restrictor::blocked_inline(ctx).await {
            let _ = call.answer().edit(InputMessage::new().html(html)).await;
            return Ok(true);
        }
    }

    if data == "heroku:mod:list" || data.starts_with("heroku:mod:list:") {
        let page = data
            .strip_prefix("heroku:mod:list:")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let empty = MODULE_CACHE
            .lock()
            .map(|cache| cache.is_empty())
            .unwrap_or(true);
        if empty {
            if let Err(err) = refresh_module_cache(ctx).await {
                let _ = call.answer().alert(format!("{err:#}")).send().await;
                return Ok(true);
            }
        }
        show_gallery_call(ctx, call, page).await;
        return Ok(true);
    }

    if let Some(raw_index) = data.strip_prefix("heroku:mod:in:") {
        let item = raw_index.parse::<usize>().ok().and_then(|index| {
            MODULE_CACHE
                .lock()
                .ok()
                .and_then(|cache| cache.get(index).cloned())
        });
        let Some((repo, entry)) = item else {
            let _ = call
                .answer()
                .alert(ctx.tr("loader.module_not_found"))
                .send()
                .await;
            return Ok(true);
        };

        let _ = call
            .answer()
            .text(ctx.tr("loader.loading_module"))
            .send()
            .await;

        let installed = install_entry(ctx, &repo, &entry).await;
        let rust = matches!(&installed, Ok(path) if is_rust_module(path));
        let (html, buttons) = match installed {
            Ok(path) => (
                ctx.tf(
                    "loader.module_installed",
                    &[&escape_html(&display_name(&path))],
                ),
                vec![vec![Button::data(
                    ctx.tr("loader.gallery_back"),
                    b"heroku:mod:list:0".to_vec(),
                )]],
            ),
            Err(err) => (
                ctx.tf(
                    "loader.module_install_failed",
                    &[&escape_html(&format!("{err:#}"))],
                ),
                vec![vec![Button::data(
                    ctx.tr("loader.gallery_back"),
                    b"heroku:mod:list:0".to_vec(),
                )]],
            ),
        };
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
        if rust {
            finish_rust_change(ctx, None, true).await?;
        }
        return Ok(true);
    }

    if data == "heroku:mod:all" {
        let _ = call
            .answer()
            .text(ctx.tr("loader.installing_all"))
            .send()
            .await;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let repo = primary_repo(&ctx);
            let result = async {
                let entries = fetch_modules(&ctx, &repo).await?;
                let mut installed = 0usize;
                let mut failed = 0usize;
                let mut rust = false;
                for entry in entries {
                    if let Ok(path) = install_entry(&ctx, &repo, &entry).await {
                        installed += 1;
                        rust |= is_rust_module(&path);
                    } else {
                        failed += 1;
                    }
                }
                Ok::<(usize, usize, bool), anyhow::Error>((installed, failed, rust))
            }
            .await;

            let (html, rust) = match result {
                Ok((installed, 0, rust)) => (
                    ctx.tf("loader.installed_all", &[&installed.to_string()]),
                    rust,
                ),
                Ok((installed, failed, rust)) => (
                    ctx.tf(
                        "loader.installed_partial",
                        &[&installed.to_string(), &failed.to_string()],
                    ),
                    rust,
                ),
                Err(err) => (
                    ctx.tf(
                        "loader.module_install_failed",
                        &[&escape_html(&format!("{err:#}"))],
                    ),
                    false,
                ),
            };
            let _ = ctx
                .inline
                .send_to_user(
                    &ctx,
                    ctx.me_id,
                    html,
                    vec![vec![
                        Button::data(ctx.tr("loader.gallery_back"), b"heroku:mod:list:0".to_vec()),
                        Button::data("✖️ Close", b"heroku:close".to_vec()),
                    ]],
                )
                .await;
            if rust {
                let _ = finish_rust_change(&ctx, None, true).await;
            }
        });
        return Ok(true);
    }

    if let Some(raw_index) = data.strip_prefix("heroku:mod:repo:") {
        let repo = raw_index
            .parse::<usize>()
            .ok()
            .and_then(|index| repos(ctx).get(index).cloned());
        let Some(repo) = repo else {
            let _ = call.answer().alert(ctx.tr("loader.no_repo")).send().await;
            return Ok(true);
        };
        let _ = call
            .answer()
            .text(ctx.tf("loader.installing_all", &[&escape_html(&repo)]))
            .send()
            .await;
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let (html, rust) = install_repo_all(&ctx, &repo).await;
            let _ = ctx
                .inline
                .send_to_user(
                    &ctx,
                    ctx.me_id,
                    html,
                    vec![vec![
                        Button::data(ctx.tr("loader.gallery_back"), b"heroku:mod:list:0".to_vec()),
                        Button::data("✖️ Close", b"heroku:close".to_vec()),
                    ]],
                )
                .await;
            if rust {
                let _ = finish_rust_change(&ctx, None, true).await;
            }
        });
        return Ok(true);
    }

    if data == "heroku:mod:clear" {
        let html = ctx.tr("loader.confirm_clearmodules");
        let buttons = vec![
            vec![Button::data(
                ctx.tr("loader.gallery_clear_yes"),
                b"heroku:mod:clear!".to_vec(),
            )],
            vec![Button::data("✖️ Cancel", b"heroku:close".to_vec())],
        ];
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
        return Ok(true);
    }

    if data == "heroku:mod:clear!" {
        let had_rust = has_rust_modules(ctx);
        let html = match clear_modules(ctx) {
            Ok(removed) => ctx.tf("loader.modules_unloaded", &[&removed.to_string()]),
            Err(err) => ctx.tf(
                "loader.module_install_failed",
                &[&escape_html(&format!("{err:#}"))],
            ),
        };
        let buttons = vec![vec![Button::data(
            ctx.tr("loader.gallery_back"),
            b"heroku:mod:list:0".to_vec(),
        )]];
        let _ = call
            .answer()
            .edit(
                InputMessage::new()
                    .html(html)
                    .reply_markup(ReplyMarkup::from_buttons(&buttons)),
            )
            .await;
        if had_rust {
            finish_rust_change(ctx, None, true).await?;
        }
        return Ok(true);
    }

    Ok(false)
}

async fn install_repo_all(ctx: &Arc<Ctx>, repo: &str) -> (String, bool) {
    let entries = match fetch_modules(ctx, repo).await {
        Ok(entries) => entries,
        Err(err) => {
            return (
                ctx.tf(
                    "loader.module_install_failed",
                    &[&escape_html(&format!("{err:#}"))],
                ),
                false,
            )
        }
    };
    if entries.is_empty() {
        return (ctx.tr("loader.no_modules_in_repo"), false);
    }

    let mut installed = 0usize;
    let mut rust = false;
    let mut failed = Vec::new();
    for entry in entries {
        match install_entry(ctx, repo, &entry).await {
            Ok(path) => {
                installed += 1;
                rust |= is_rust_module(&path);
            }
            Err(err) => failed.push(format!(
                "▫️ <code>{}</code> — {}",
                escape_html(&entry),
                escape_html(&err.to_string())
            )),
        }
    }

    let html = if failed.is_empty() {
        ctx.tf("loader.installed_all", &[&installed.to_string()])
    } else {
        let mut text = ctx.tf(
            "loader.installed_partial",
            &[&installed.to_string(), &failed.len().to_string()],
        );
        text.push('\n');
        text.push_str(&failed.join("\n"));
        text
    };
    (html, rust)
}

fn module_stem(entry: &str) -> String {
    let name = entry_name(entry);
    for ext in [".py", ".rhai", ".rs"] {
        if let Some(stem) = name.strip_suffix(ext) {
            return stem.to_owned();
        }
    }
    name
}

fn repo_host(repo: &str) -> String {
    repo.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(repo)
        .to_owned()
}

fn primary_repo(ctx: &Arc<Ctx>) -> String {
    normalize_repo(&ctx.db.get_str(NS, "MODULES_REPO", DEFAULT_REPO))
        .unwrap_or_else(|| DEFAULT_REPO.to_owned())
}

fn additional_repos(ctx: &Arc<Ctx>) -> Vec<String> {
    ctx.db
        .get_string_list(NS, "ADDITIONAL_REPOS")
        .into_iter()
        .filter_map(|repo| normalize_repo(&repo))
        .collect()
}

fn set_additional_repos(ctx: &Arc<Ctx>, repos: &[String]) {
    ctx.db.set(
        NS,
        "ADDITIONAL_REPOS",
        serde_json::Value::Array(
            repos
                .iter()
                .map(|repo| serde_json::Value::String(repo.clone()))
                .collect(),
        ),
    );
}

fn repos(ctx: &Arc<Ctx>) -> Vec<String> {
    let mut out = vec![primary_repo(ctx)];
    for repo in additional_repos(ctx) {
        if !out.contains(&repo) {
            out.push(repo);
        }
    }
    out.retain(|repo| repo.starts_with("http"));
    out
}

fn normalize_repo(raw: &str) -> Option<String> {
    let mut repo = raw
        .trim()
        .trim_matches('`')
        .trim_end_matches('/')
        .to_owned();
    if repo.is_empty() {
        return None;
    }
    if !repo.starts_with("http://") && !repo.starts_with("https://") {
        repo = format!("https://{repo}");
    }
    let rest = repo
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    if rest.is_empty() || !rest.contains('.') || rest.contains(char::is_whitespace) {
        return None;
    }
    Some(repo)
}

fn auth_header(ctx: &Arc<Ctx>) -> Option<String> {
    let raw = ctx.db.get_str(NS, "basic_auth", "");
    let (user, password) = raw.split_once(':')?;
    if user.is_empty() {
        return None;
    }
    let token = base64_encode(format!("{user}:{password}").as_bytes());
    Some(format!("Basic {token}"))
}

fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("Heroku/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("failed to build HTTP client")
}

async fn fetch_text(ctx: &Arc<Ctx>, url: &str) -> Result<String> {
    let mut request = http_client()?.get(url);
    if let Some(auth) = auth_header(ctx) {
        request = request.header(reqwest::header::AUTHORIZATION, auth);
    }
    let response = request.send().await.context("request failed")?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    response.text().await.context("failed to read response")
}

async fn fetch_modules(ctx: &Arc<Ctx>, repo: &str) -> Result<Vec<String>> {
    let url = format!("{}/full.txt", repo.trim_end_matches('/'));
    let text = fetch_text(ctx, &url).await?;
    let list: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect();
    Ok(list)
}

fn has_known_extension(name: &str) -> bool {
    [".py", ".rhai", ".rs"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

fn entry_name(entry: &str) -> String {
    let base = entry.rsplit('/').next().unwrap_or(entry).trim();
    if has_known_extension(base) {
        base.to_owned()
    } else {
        format!("{base}.py")
    }
}

fn module_url(repo: &str, entry: &str) -> String {
    let trimmed = repo.trim_end_matches('/');
    let entry = entry.trim_start_matches('/');
    if has_known_extension(entry) {
        format!("{trimmed}/{entry}")
    } else {
        format!("{trimmed}/{entry}.py")
    }
}

async fn find_module(ctx: &Arc<Ctx>, name: &str) -> Result<Option<(String, String)>> {
    let needle = sanitize_stem(name).to_lowercase();
    for repo in repos(ctx) {
        let modules = match fetch_modules(ctx, &repo).await {
            Ok(modules) => modules,
            Err(_) => continue,
        };
        for entry in modules {
            let file = entry_name(&entry);
            if file.trim_end_matches(".py").to_lowercase() == needle {
                return Ok(Some((repo, entry)));
            }
        }
    }
    Ok(None)
}

async fn install_entry(ctx: &Arc<Ctx>, repo: &str, entry: &str) -> Result<PathBuf> {
    let file = entry_name(entry);
    ensure_allowed(ctx, &file)?;
    let url = module_url(repo, entry);
    download_to_modules(ctx, &url, &file).await
}

async fn download_to_modules(ctx: &Arc<Ctx>, url: &str, file_name: &str) -> Result<PathBuf> {
    let dir = modules_dir(ctx);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let mut request = http_client()?.get(url);
    if let Some(auth) = auth_header(ctx) {
        request = request.header(reqwest::header::AUTHORIZATION, auth);
    }
    let response = request.send().await.context("download failed")?;
    if !response.status().is_success() {
        anyhow::bail!("HTTP {}", response.status());
    }
    if response
        .content_length()
        .map(|size| size as usize > MAX_MODULE_BYTES)
        .unwrap_or(false)
    {
        anyhow::bail!("module is too large");
    }
    let bytes = response.bytes().await.context("failed to read module")?;
    if bytes.len() > MAX_MODULE_BYTES {
        anyhow::bail!("module is too large");
    }
    if bytes.is_empty() {
        anyhow::bail!("empty response");
    }

    let path = dir.join(sanitize_file_name(file_name));
    crate::config::write_private(&path, &bytes)?;
    if is_rust_module(&path) {
        mirror_repo_module(&path)?;
    }
    Ok(path)
}

fn ensure_allowed(ctx: &Arc<Ctx>, file: &str) -> Result<()> {
    let stem = sanitize_stem(file);
    let denied = ctx.db.get_string_list("LoaderRestrictor", "denied");
    if denied.iter().any(|item| item.eq_ignore_ascii_case(&stem)) {
        anyhow::bail!("module is denied by LoaderRestrictor");
    }
    Ok(())
}

fn sanitize_stem(name: &str) -> String {
    let base = name
        .rsplit('/')
        .next()
        .unwrap_or(name)
        .trim()
        .trim_end_matches(".py")
        .to_owned();
    base.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .collect()
}

fn sanitize_file_name(name: &str) -> String {
    let mut file = sanitize_stem(name);
    if file.is_empty() {
        file = format!("module-{:08x}", rand::thread_rng().gen::<u32>());
    }
    if !has_known_extension(&file) {
        file.push_str(".py");
    }
    file
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("module.py")
        .to_owned()
}

fn find_local_module(ctx: &Arc<Ctx>, name: &str) -> Option<PathBuf> {
    let stem = sanitize_stem(name).to_lowercase();
    if stem.is_empty() {
        return None;
    }
    list_local_modules(ctx).into_iter().find(|path| {
        display_name(path)
            .trim_end_matches(".py")
            .trim_end_matches(".rhai")
            .trim_end_matches(".rs")
            .to_lowercase()
            == stem
    })
}

fn clear_modules(ctx: &Arc<Ctx>) -> Result<usize> {
    let files = list_local_modules(ctx);
    let mut removed = 0;
    for path in files {
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

fn is_rust_module(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("rs")
}

pub(crate) fn has_rust_modules(ctx: &Arc<Ctx>) -> bool {
    list_local_modules(ctx)
        .iter()
        .any(|path| is_rust_module(path))
}

fn repo_modules_dir() -> Option<PathBuf> {
    crate::modules::updater::repo_root().map(|root| root.join("modules"))
}

/// External Rust modules must live inside the repository so `build.rs` can see
/// them when the release is rebuilt.
fn mirror_repo_module(path: &Path) -> Result<()> {
    let Some(dir) = repo_modules_dir() else {
        return Ok(());
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let Some(name) = path.file_name() else {
        return Ok(());
    };
    let dest = dir.join(name);
    if dest != path {
        std::fs::copy(path, &dest)
            .with_context(|| format!("failed to copy module to {}", dest.display()))?;
    }
    Ok(())
}

fn remove_repo_module(path: &Path) {
    let Some(dir) = repo_modules_dir() else {
        return;
    };
    if let Some(name) = path.file_name() {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

fn clear_repo_modules() {
    let Some(dir) = repo_modules_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_rust_module(&path) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Rebuild the release after adding or removing Rust modules and restart so
/// the freshly compiled registry is loaded.
pub(crate) async fn finish_rust_change(
    ctx: &Arc<Ctx>,
    message: Option<&Msg>,
    force: bool,
) -> Result<()> {
    if !force && !has_rust_modules(ctx) {
        return Ok(());
    }
    let Some(root) = crate::modules::updater::repo_root() else {
        notify_rust(ctx, message, ctx.tr("loader.rust_no_repo")).await?;
        return Ok(());
    };
    // Keep the repository copies in sync before building.
    if has_rust_modules(ctx) {
        for path in list_local_modules(ctx) {
            if is_rust_module(&path) {
                let _ = mirror_repo_module(&path);
            }
        }
    } else {
        clear_repo_modules();
    }
    notify_rust(ctx, message, ctx.tr("loader.rust_building")).await?;
    match crate::modules::updater::build_release(&root).await {
        Ok(()) => {
            notify_rust(ctx, message, ctx.tr("loader.rust_built")).await?;
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(1200)).await;
                if let Err(err) = crate::inline::restart_process() {
                    tracing::error!("Restart after building Rust modules failed: {err}");
                }
            });
        }
        Err(err) => {
            let html = ctx.tf("loader.rust_failed", &[&escape_html(&format!("{err:#}"))]);
            notify_rust(ctx, message, html).await?;
        }
    }
    Ok(())
}

async fn notify_rust(ctx: &Arc<Ctx>, message: Option<&Msg>, html: String) -> Result<()> {
    match message {
        Some(message) => message.reply_html(html).await.map(|_| ()),
        None => ctx.send_self(html).await,
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(LoaderMod))
}

inventory::submit! {
    ModuleFactory(|| Box::new(LoaderRestrictor))
}
