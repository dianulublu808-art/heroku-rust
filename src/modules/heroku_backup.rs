// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Database backups and restores.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use grammers_client::message::Button;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};
use crate::ziparch;

pub struct HerokuBackupMod;

const PERIOD_IMAGE: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/unit_alpha.jpg";

#[async_trait]
impl Module for HerokuBackupMod {
    fn name(&self) -> &'static str {
        "HerokuBackupMod"
    }

    fn description(&self) -> &'static str {
        "Back up and restore the database"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("backupdb", "Send a database backup"),
            CommandDef::new("restoredb", "Restore the database from a replied JSON file"),
            CommandDef::new("backupall", "Send a full backup"),
            CommandDef::new("restoreall", "Restore a full backup"),
            CommandDef::new("backupmods", "Send an archive of downloaded modules"),
            CommandDef::new(
                "restoremods",
                "Restore downloaded modules from a replied archive",
            ),
            CommandDef::new("set_backup_period", "Set automatic backup period").usage("<hours>"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![ConfigSpec::new(
            "backup_period",
            "Automatic backup period in seconds (0 disables)",
            ConfigDefault::Int(0),
            ConfigKind::Int,
        )]
    }

    async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
        // Python's `inline_ready` proposes the backup period on the first run
        // with the `unit_alpha.jpg` photo card. The prompt is skipped once the
        // owner has picked a period (including "Never", stored as 0).
        if ctx.db.get_value("HerokuBackupMod", "period").is_none() {
            let setup_ctx = ctx.clone();
            tokio::spawn(async move {
                let (html, buttons) = period_prompt_card(&setup_ctx);
                if setup_ctx
                    .inline
                    .send_photo_to_user(
                        &setup_ctx,
                        setup_ctx.me_id,
                        PERIOD_IMAGE,
                        html.clone(),
                        buttons,
                    )
                    .await
                    .is_err()
                {
                    let _ = setup_ctx.send_self(html).await;
                }
            });
        }

        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let period = backup_period(&ctx);
                if period <= 0 {
                    continue;
                }
                let now = chrono::Utc::now().timestamp();
                let last = ctx.db.get_i64("HerokuBackupMod", "last_backup", 0);
                if now - last < period {
                    continue;
                }
                ctx.db.set("HerokuBackupMod", "last_backup", now);
                match create_backup(&ctx) {
                    Ok(path) => {
                        let caption = ctx.tr("backup.auto_caption");
                        match send_backup_to_channel(&ctx, &path, caption, Vec::new()).await {
                            Ok(Some(link)) => {
                                let _ =
                                    ctx.send_self(ctx.tf("backup.sent_channel", &[&link])).await;
                            }
                            Ok(None) => {}
                            Err(err) => {
                                tracing::error!("Failed to send automatic backup: {err:#}")
                            }
                        }
                    }
                    Err(err) => tracing::error!("Automatic backup failed: {err:#}"),
                }
            }
        });
        Ok(())
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        match command {
            "backupdb" => {
                message.reply_html(ctx.tr("backup.creating")).await?;
                let path = create_backup(ctx)?;
                let prefix = ctx.prefixes(ctx.me_id).first().cloned().unwrap_or_default();
                let caption = ctx.tf("backup.channel_caption", &[&escape_html(&prefix)]);
                match send_backup_to_channel(ctx, &path, caption, Vec::new()).await {
                    Ok(Some(link)) => {
                        message
                            .reply_html(ctx.tf("backup.sent_channel", &[&link]))
                            .await?;
                    }
                    Ok(None) | Err(_) => {
                        message
                            .respond_file(
                                &ctx.client,
                                &path,
                                ctx.tf(
                                    "backup.created",
                                    &[&escape_html(
                                        path.file_name()
                                            .and_then(|name| name.to_str())
                                            .unwrap_or("backup.json"),
                                    )],
                                ),
                            )
                            .await?;
                    }
                }
            }
            "backupall" => self.backup_all(ctx, message).await?,
            "restoredb" => self.restore_db(ctx, message).await?,
            "restoreall" => self.restore_all(ctx, message).await?,
            "backupmods" => self.backup_modules(ctx, message).await?,
            "restoremods" => self.restore_modules(ctx, message).await?,
            "set_backup_period" => {
                let Some(raw) = args.get(0) else {
                    let user_id = message.sender_id().unwrap_or(ctx.me_id);
                    let (html, buttons) = period_card(ctx);
                    if ctx
                        .inline
                        .send_to_user(ctx, user_id, html, buttons)
                        .await
                        .is_ok()
                    {
                        return Ok(());
                    }
                    message.reply_html(ctx.tr("backup.period_invalid")).await?;
                    return Ok(());
                };
                let Ok(hours) = raw.trim().parse::<i64>() else {
                    message.reply_html(ctx.tr("backup.period_invalid")).await?;
                    return Ok(());
                };
                if !(0..200).contains(&hours) {
                    message.reply_html(ctx.tr("backup.period_invalid")).await?;
                    return Ok(());
                }
                let text = apply_period(ctx, hours);
                message.reply_html(text).await?;
            }
            _ => {
                message.reply_html(ctx.tr("common.not_found")).await?;
            }
        }
        Ok(())
    }
}

impl HerokuBackupMod {
    async fn backup_modules(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        message
            .reply_html(ctx.tr("backup.creating_modules"))
            .await?;

        let modules = local_module_files(ctx);
        let mut entries: Vec<(String, Vec<u8>)> = modules
            .iter()
            .map(|(name, content)| (name.clone(), content.as_bytes().to_vec()))
            .collect();
        entries.push((
            "db_mods.json".to_owned(),
            serde_json::to_vec_pretty(&loaded_modules(ctx))?,
        ));
        let archive = ziparch::write_zip(&entries);

        let dir = PathBuf::from(&ctx.data_root).join("backups");
        std::fs::create_dir_all(&dir)?;
        let name = format!("mods-{}.zip", chrono::Local::now().format("%d-%m-%Y-%H-%M"));
        let path = dir.join(name);
        crate::config::write_private(&path, &archive)?;

        let count = modules.len().to_string();
        let caption = ctx.tf("backup.modules_backup", &[&count]);
        match send_backup_to_channel(ctx, &path, caption.clone(), Vec::new()).await {
            Ok(Some(link)) => {
                message
                    .reply_html(ctx.tf("backup.sent_channel", &[&link]))
                    .await?;
            }
            Ok(None) | Err(_) => {
                message.respond_file(&ctx.client, &path, caption).await?;
            }
        }
        Ok(())
    }

    async fn backup_all(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        message.reply_html(ctx.tr("backup.creating_all")).await?;

        let modules = local_module_files(ctx);
        let mut mods_entries: Vec<(String, Vec<u8>)> = modules
            .iter()
            .map(|(name, content)| (name.clone(), content.as_bytes().to_vec()))
            .collect();
        mods_entries.push((
            "db_mods.json".to_owned(),
            serde_json::to_vec_pretty(&loaded_modules(ctx))?,
        ));
        let mods_zip = ziparch::write_zip(&mods_entries);

        let entries = vec![
            ("db.json".to_owned(), ctx.db.dump_pretty().into_bytes()),
            ("mods.zip".to_owned(), mods_zip),
        ];

        let archive = ziparch::write_zip(&entries);
        let dir = PathBuf::from(&ctx.data_root).join("backups");
        std::fs::create_dir_all(&dir)?;
        let name = format!(
            "backup-{}.backup",
            chrono::Local::now().format("%d-%m-%Y-%H-%M")
        );
        let path = dir.join(name);
        crate::config::write_private(&path, &archive)?;

        let prefix = ctx.prefixes(ctx.me_id).first().cloned().unwrap_or_default();
        let caption = ctx.tf("backup.channel_all_caption", &[&escape_html(&prefix)]);
        let buttons = vec![vec![Button::data(
            "↪️ Restore this",
            b"heroku:backupall:restore:confirm".to_vec(),
        )]];
        match send_backup_to_channel(ctx, &path, caption, buttons).await {
            Ok(Some(link)) => {
                message
                    .reply_html(ctx.tf("backup.all_sent_channel", &[&link]))
                    .await?;
            }
            Ok(None) | Err(_) => {
                message
                    .respond_file(
                        &ctx.client,
                        &path,
                        ctx.tf("backup.all_created", &[&modules.len().to_string()]),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn restore_modules(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let Some(reply) = message.get_reply().await? else {
            message.reply_html(ctx.tr("backup.no_file")).await?;
            return Ok(());
        };
        if reply.inner.media().is_none() {
            message.reply_html(ctx.tr("backup.no_file")).await?;
            return Ok(());
        }

        let path = PathBuf::from(&ctx.data_root)
            .join("backups")
            .join(format!("restore-mods-{}.archive", message.id()));
        message
            .reply_html(ctx.tr("backup.restoring_modules"))
            .await?;
        reply
            .download_media(&path)
            .await
            .context("failed to download the modules archive")?;
        let raw = tokio::fs::read(&path)
            .await
            .context("failed to read the modules archive")?;
        let _ = tokio::fs::remove_file(&path).await;

        // Python's `backupmods` produces a ZIP, while older Rust builds wrote
        // a JSON object with the same data. Accept both formats.
        let restored = if raw.starts_with(b"PK\x03\x04") {
            restore_modules_zip(ctx, &raw)?
        } else {
            let value: Value = serde_json::from_slice(&raw).context("invalid modules archive")?;
            if value.get("modules").is_some() {
                let (modules, primary_repo, repos, basic_auth) = parse_modules_archive(&value)?;
                let restored = restore_module_files(ctx, &modules)?;
                if let Some(repo) = primary_repo {
                    ctx.db.set("LoaderMod", "MODULES_REPO", repo);
                }
                if let Some(repos) = repos {
                    ctx.db.set(
                        "LoaderMod",
                        "ADDITIONAL_REPOS",
                        Value::Array(repos.into_iter().map(Value::String).collect()),
                    );
                }
                if let Some(auth) = basic_auth {
                    ctx.db.set("LoaderMod", "basic_auth", auth);
                }
                restored
            } else if restore_loaded_modules(ctx, &value) {
                0
            } else {
                message.reply_html(ctx.tr("backup.bad_file")).await?;
                return Ok(());
            }
        };

        message
            .reply_html(ctx.tf("backup.mods_restored", &[&restored.to_string()]))
            .await?;
        restart_or_rebuild(ctx, message).await?;
        Ok(())
    }

    async fn restore_db(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let Some(path) = download_backup(ctx, message, "db").await? else {
            return Ok(());
        };
        let raw = tokio::fs::read(&path)
            .await
            .context("failed to read the backup")?;
        let _ = tokio::fs::remove_file(&path).await;

        let value = if raw.starts_with(b"PK\x03\x04") {
            let mut entries = ziparch::read_zip(&raw).context("invalid backup archive")?;
            let Some(db) = entries
                .remove("db.json")
                .or_else(|| entries.remove("database.json"))
            else {
                message.reply_html(ctx.tr("backup.bad_file")).await?;
                return Ok(());
            };
            serde_json::from_slice::<Value>(&db).context("invalid database JSON")?
        } else {
            serde_json::from_slice::<Value>(&raw).context("invalid JSON")?
        };

        if !value.is_object() {
            message.reply_html(ctx.tr("backup.bad_file")).await?;
            return Ok(());
        }

        ctx.db.restore(sanitize_db(value));
        message.reply_html(ctx.tr("backup.restored")).await?;
        schedule_restart();
        Ok(())
    }

    async fn restore_all(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let Some(path) = download_backup(ctx, message, "all").await? else {
            return Ok(());
        };
        let raw = tokio::fs::read(&path)
            .await
            .context("failed to read the backup")?;
        let _ = tokio::fs::remove_file(&path).await;

        if raw.starts_with(b"PK\x03\x04") {
            let outcome = apply_full_archive(ctx, &raw)?;
            if !outcome.database && outcome.modules == 0 && !outcome.mods {
                message.reply_html(ctx.tr("backup.bad_file")).await?;
                return Ok(());
            }

            message
                .reply_html(ctx.tf(
                    "backup.all_restored",
                    &[
                        if outcome.database { "yes" } else { "no" },
                        &outcome.modules.to_string(),
                    ],
                ))
                .await?;
            restart_or_rebuild(ctx, message).await?;
            return Ok(());
        }

        let value: Value = serde_json::from_slice(&raw).context("invalid JSON")?;
        if !value.is_object() {
            message.reply_html(ctx.tr("backup.bad_file")).await?;
            return Ok(());
        }

        if value.get("modules").is_some() {
            let (modules, primary_repo, repos, basic_auth) = parse_modules_archive(&value)?;
            let restored = restore_module_files(ctx, &modules)?;
            let mut loader = serde_json::Map::new();
            if let Some(repo) = primary_repo {
                loader.insert("primary_repo".to_owned(), Value::String(repo));
            }
            if let Some(repos) = repos {
                loader.insert(
                    "repos".to_owned(),
                    Value::Array(repos.into_iter().map(Value::String).collect()),
                );
            }
            if let Some(auth) = basic_auth {
                loader.insert("basic_auth".to_owned(), Value::String(auth));
            }
            apply_loader_config(ctx, &Value::Object(loader));
            message
                .reply_html(ctx.tf("backup.mods_restored", &[&restored.to_string()]))
                .await?;
            restart_or_rebuild(ctx, message).await?;
            return Ok(());
        }

        ctx.db.restore(sanitize_db(value));
        message.reply_html(ctx.tr("backup.restored")).await?;
        restart_or_rebuild(ctx, message).await?;
        Ok(())
    }
}

/// Restore is a two-step process: `.rs` modules are compiled before restart,
/// everything else restarts immediately.
async fn restart_or_rebuild(ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
    if crate::modules::loader::has_rust_modules(ctx) {
        crate::modules::loader::finish_rust_change(ctx, Some(message), false).await
    } else {
        schedule_restart();
        Ok(())
    }
}

async fn download_backup(ctx: &Arc<Ctx>, message: &Msg, kind: &str) -> Result<Option<PathBuf>> {
    let Some(reply) = message.get_reply().await? else {
        message.reply_html(ctx.tr("backup.no_file")).await?;
        return Ok(None);
    };
    if reply.inner.media().is_none() {
        message.reply_html(ctx.tr("backup.no_file")).await?;
        return Ok(None);
    }

    let key = if kind == "all" {
        "backup.restoring_all"
    } else {
        "backup.restoring"
    };
    message.reply_html(ctx.tr(key)).await?;

    let path = PathBuf::from(&ctx.data_root)
        .join("backups")
        .join(format!("restore-{kind}-{}.backup", message.id()));
    reply
        .download_media(&path)
        .await
        .context("failed to download the backup")?;
    Ok(Some(path))
}

fn sanitize_db(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        if let Some(Value::Object(inline)) = object.get_mut("heroku.inline") {
            for key in ["bot_token", "custom_bot", "bot_id"] {
                inline.remove(key);
            }
        }
    }
    value
}

fn local_module_files(ctx: &Arc<Ctx>) -> BTreeMap<String, String> {
    let mut modules = BTreeMap::new();
    let dir = crate::modules::loader::modules_dir(ctx);
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !is_module_file(name) {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            modules.insert(name.to_owned(), raw);
        }
    }
    modules
}

fn is_module_file(name: &str) -> bool {
    let name = name.trim();
    !name.is_empty()
        && !name.ends_with('/')
        && (name.ends_with(".py") || name.ends_with(".rhai"))
        && name != "db_mods.json"
}

/// Python-compatible `LoaderMod.loaded_modules` mapping (module -> source URL).
fn loaded_modules(ctx: &Ctx) -> Value {
    match ctx.db.get_value("LoaderMod", "loaded_modules") {
        Some(Value::Object(map)) => Value::Object(map),
        _ => Value::Object(serde_json::Map::new()),
    }
}

/// Store `loaded_modules` from a `db_mods.json` payload. Returns `true` when
/// the payload was a valid mapping of strings.
fn restore_loaded_modules(ctx: &Ctx, value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    if !map.values().all(Value::is_string) {
        return false;
    }
    ctx.db
        .set("LoaderMod", "loaded_modules", Value::Object(map.clone()));
    true
}

fn restore_modules_zip(ctx: &Arc<Ctx>, raw: &[u8]) -> Result<usize> {
    let mut entries = ziparch::read_zip(raw).context("invalid modules archive")?;
    if let Some(mods) = entries.remove("db_mods.json") {
        if let Ok(value) = serde_json::from_slice::<Value>(&mods) {
            restore_loaded_modules(ctx, &value);
        }
    }

    let mut modules = BTreeMap::new();
    for (name, content) in entries {
        if !is_module_file(&name) {
            continue;
        }
        let Some(name) = PathBuf::from(&name)
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        modules.insert(name, String::from_utf8_lossy(&content).into_owned());
    }
    if modules.is_empty() {
        anyhow::bail!("archive contains no modules");
    }
    restore_module_files(ctx, &modules)
}

fn apply_loader_config(ctx: &Arc<Ctx>, value: &Value) {
    if let Some(repo) = value.get("primary_repo").and_then(Value::as_str) {
        ctx.db.set("LoaderMod", "MODULES_REPO", repo);
    }
    if let Some(repos) = value.get("repos").and_then(Value::as_array) {
        ctx.db.set(
            "LoaderMod",
            "ADDITIONAL_REPOS",
            Value::Array(
                repos
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|repo| Value::String(repo.to_owned()))
                    .collect(),
            ),
        );
    }
    if let Some(auth) = value.get("basic_auth").and_then(Value::as_str) {
        ctx.db.set("LoaderMod", "basic_auth", auth);
    }
}

fn restore_module_files(ctx: &Arc<Ctx>, modules: &BTreeMap<String, String>) -> Result<usize> {
    let dir = crate::modules::loader::modules_dir(ctx);
    std::fs::create_dir_all(&dir)?;
    let mut restored = 0usize;
    for (name, content) in modules {
        if name == "db_mods.json" {
            continue;
        }
        let Some(file_name) = PathBuf::from(name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        crate::config::write_private(&dir.join(file_name), content.as_bytes())?;
        restored += 1;
    }
    Ok(restored)
}

fn schedule_restart() {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(800)).await;
        if let Err(err) = crate::inline::restart_process() {
            tracing::error!("Restart after restoring a backup failed: {err}");
        }
    });
}

type ModulesArchive = (
    std::collections::BTreeMap<String, String>,
    Option<String>,
    Option<Vec<String>>,
    Option<String>,
);

fn parse_modules_archive(value: &Value) -> Result<ModulesArchive> {
    let mut modules = std::collections::BTreeMap::new();

    let modules_value = value.get("modules").unwrap_or(value);
    let Some(object) = modules_value.as_object() else {
        anyhow::bail!("invalid modules archive");
    };
    for (name, content) in object {
        let Some(content) = content.as_str() else {
            continue;
        };
        modules.insert(name.clone(), content.to_owned());
    }

    if modules.is_empty() {
        anyhow::bail!("archive contains no modules");
    }

    let primary_repo = value
        .get("primary_repo")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let repos = value.get("repos").and_then(Value::as_array).map(|repos| {
        repos
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    });
    let basic_auth = value
        .get("basic_auth")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);

    Ok((modules, primary_repo, repos, basic_auth))
}

pub(crate) struct FullRestore {
    database: bool,
    modules: usize,
    mods: bool,
}

fn apply_full_archive(ctx: &Arc<Ctx>, raw: &[u8]) -> Result<FullRestore> {
    let mut entries = ziparch::read_zip(raw).context("invalid backup archive")?;
    let db = entries
        .remove("db.json")
        .or_else(|| entries.remove("database.json"));
    let loader = entries.remove("loader.json");
    let nested = entries.remove("mods.zip");

    let mut modules = BTreeMap::new();
    let mut db_mods: Option<Value> = None;
    if let Some(nested) = nested {
        match ziparch::read_zip(&nested) {
            Ok(mut files) => {
                if let Some(raw) = files.remove("db_mods.json") {
                    db_mods = serde_json::from_slice::<Value>(&raw).ok();
                }
                for (name, content) in files {
                    if !is_module_file(&name) {
                        continue;
                    }
                    let Some(name) = PathBuf::from(&name)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(ToOwned::to_owned)
                    else {
                        continue;
                    };
                    modules.insert(name, String::from_utf8_lossy(&content).into_owned());
                }
            }
            Err(err) => tracing::warn!("Failed to read nested mods.zip: {err:#}"),
        }
    }
    // Older Rust backups stored modules as `modules/<name>` entries.
    for (name, content) in entries {
        let Some(name) = name.strip_prefix("modules/") else {
            continue;
        };
        if !is_module_file(name) {
            continue;
        }
        let Some(name) = PathBuf::from(name)
            .file_name()
            .and_then(|name| name.to_str())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        modules.insert(name, String::from_utf8_lossy(&content).into_owned());
    }

    let mut database = false;
    if let Some(db) = db {
        match serde_json::from_slice::<Value>(&db) {
            Ok(value) if value.is_object() => {
                ctx.db.restore(sanitize_db(value));
                database = true;
            }
            Ok(_) => {}
            Err(err) => tracing::warn!("Invalid database in backup: {err}"),
        }
    }
    if let Some(loader) = loader {
        if let Ok(value) = serde_json::from_slice::<Value>(&loader) {
            apply_loader_config(ctx, &value);
        }
    }
    // `db_mods.json` must be applied after the database restore, because
    // restoring replaces the whole database.
    let mods = db_mods
        .as_ref()
        .map(|value| restore_loaded_modules(ctx, value))
        .unwrap_or(false);
    let modules = restore_module_files(ctx, &modules)?;

    Ok(FullRestore {
        database,
        modules,
        mods,
    })
}

/// Handle `heroku:backupall:restore[:confirm]` pressed on a backup file that
/// the inline bot posted into the Backups topic.
pub(crate) async fn handle_restore_callback(
    ctx: &Arc<Ctx>,
    call: &grammers_client::update::CallbackQuery,
    data: &str,
) -> Result<()> {
    if data == "heroku:backupall:restore:confirm" {
        let markup = grammers_client::message::ReplyMarkup::from_buttons(&[vec![Button::data(
            ctx.tr("backup.confirm_yes"),
            b"heroku:backupall:restore".to_vec(),
        )]]);
        let _ = call
            .answer()
            .edit(
                grammers_client::message::InputMessage::new()
                    .html(ctx.tr("backup.confirm_restore"))
                    .reply_markup(markup),
            )
            .await;
        return Ok(());
    }

    let message = call.load_message().await?;
    let path = PathBuf::from(&ctx.data_root)
        .join("backups")
        .join(format!("restore-callback-{}.backup", message.id()));
    if let Err(err) = message.download_media(&path).await {
        tracing::warn!("Failed to download the channel backup: {err:#}");
        let _ = call.answer().alert(ctx.tr("backup.no_file")).send().await;
        return Ok(());
    }
    let raw = tokio::fs::read(&path)
        .await
        .context("failed to read the channel backup")?;
    let _ = tokio::fs::remove_file(&path).await;

    match apply_full_archive(ctx, &raw) {
        Ok(outcome) if outcome.database || outcome.modules > 0 || outcome.mods => {
            let _ = call
                .answer()
                .text(ctx.tr("backup.all_restored_bot"))
                .send()
                .await;
            if outcome.database || outcome.modules > 0 {
                let message = Msg::new(message);
                restart_or_rebuild(ctx, &message).await?;
            }
        }
        _ => {
            let _ = call.answer().alert(ctx.tr("backup.bad_file")).send().await;
        }
    }
    Ok(())
}

async fn send_backup_to_channel(
    ctx: &Arc<Ctx>,
    path: &std::path::Path,
    caption: String,
    buttons: Vec<Vec<Button>>,
) -> Result<Option<String>> {
    let channel = crate::content::ensure_content_channel(ctx).await?;
    let topic = crate::content::ensure_topic(
        ctx,
        crate::content::TOPIC_BACKUPS,
        crate::content::TOPIC_BACKUPS_DESCRIPTION,
    )
    .await?;
    let channel_id = channel.id.bare_id_unchecked();

    // Python always uploads backups with the inline bot, so the "Restore this"
    // button keeps working even after a userbot restart.
    match ctx
        .inline
        .send_document_to_channel(ctx, channel_id, topic, path, caption.clone(), &buttons)
        .await
    {
        Ok(id) => return Ok(Some(format!("https://t.me/c/{channel_id}/{topic}/{id}"))),
        Err(err) => {
            tracing::debug!("Inline bot could not send the backup, falling back: {err:#}");
        }
    }

    let uploaded = ctx
        .client
        .upload_file(path)
        .await
        .with_context(|| format!("failed to upload {}", path.display()))?;
    let mut input = grammers_client::message::InputMessage::new()
        .html(caption)
        .document(uploaded)
        .reply_to(Some(topic));
    if !buttons.is_empty() {
        input = input.reply_markup(grammers_client::message::ReplyMarkup::from_buttons(
            &buttons,
        ));
    }
    let sent = ctx
        .client
        .send_message(channel, input)
        .await
        .context("failed to send the backup to the content channel")?;

    Ok(Some(format!(
        "https://t.me/c/{channel_id}/{topic}/{}",
        sent.id()
    )))
}

fn create_backup(ctx: &Arc<Ctx>) -> Result<PathBuf> {
    let dir = PathBuf::from(&ctx.data_root).join("backups");
    std::fs::create_dir_all(&dir)?;
    let name = format!(
        "db-backup-{}.json",
        chrono::Local::now().format("%d-%m-%Y-%H-%M")
    );
    let path = dir.join(name);
    let raw = ctx.db.dump_pretty();
    crate::config::write_private(&path, raw.as_bytes())?;
    Ok(path)
}

/// Automatic backup period in seconds. Reads both the Python-compatible
/// `period` key and the older Rust `backup_period` key.
fn backup_period(ctx: &Ctx) -> i64 {
    let period = ctx.db.get_i64("HerokuBackupMod", "period", 0);
    if period != 0 {
        return period;
    }
    ctx.db.get_i64("HerokuBackupMod", "backup_period", 0)
}

fn apply_period(ctx: &Ctx, hours: i64) -> String {
    let seconds = hours * 3600;
    ctx.db
        .set("HerokuBackupMod", "period", Value::from(seconds));
    ctx.db
        .set("HerokuBackupMod", "backup_period", Value::from(seconds));
    ctx.db.set(
        "HerokuBackupMod",
        "last_backup",
        Value::from(chrono::Utc::now().timestamp()),
    );
    if hours == 0 {
        ctx.tr("backup.period_never")
    } else {
        ctx.tf("backup.period_set", &[&hours.to_string()])
    }
}

fn period_buttons(ctx: &Arc<Ctx>, with_close: bool) -> Vec<Vec<Button>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    for hours in [1_i64, 2, 4, 6, 8, 12, 24, 48, 168] {
        row.push(Button::data(
            format!("🕰 {hours} h"),
            format!("heroku:backup:period:{hours}").into_bytes(),
        ));
        if row.len() == 3 {
            rows.push(std::mem::take(&mut row));
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    rows.push(vec![Button::data(
        ctx.tr("backup.btn_never"),
        b"heroku:backup:period:0".to_vec(),
    )]);
    if with_close {
        rows.push(vec![Button::data("✖️ Close", b"heroku:close".to_vec())]);
    }
    rows
}

/// First-run prompt mirroring Python's `inline_ready` photo card.
pub(crate) fn period_prompt_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    (ctx.tr("backup.period_prompt"), period_buttons(ctx, false))
}

pub(crate) fn period_card(ctx: &Arc<Ctx>) -> (String, Vec<Vec<Button>>) {
    let seconds = backup_period(ctx);
    let current = if seconds > 0 {
        ctx.tf("backup.period_hours", &[&(seconds / 3600).to_string()])
    } else {
        ctx.tr("backup.period_never")
    };
    let html = ctx.tf("backup.period_title", &[&current]);
    (html, period_buttons(ctx, true))
}

pub(crate) fn set_period_from_inline(ctx: &Arc<Ctx>, hours: i64) -> (String, Vec<Vec<Button>>) {
    apply_period(ctx, hours);
    period_card(ctx)
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuBackupMod))
}
