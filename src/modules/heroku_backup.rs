// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Database backups and restores.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};
use crate::ziparch;

pub struct HerokuBackupMod;

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
            CommandDef::new("set_backup_period", "Set automatic backup period").usage("<seconds>"),
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
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let period = ctx.db.get_i64("HerokuBackupMod", "backup_period", 0);
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
                        match send_backup_to_channel(&ctx, &path, caption).await {
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
                match send_backup_to_channel(ctx, &path, caption).await {
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
                let seconds = args.get(0).and_then(|raw| raw.parse::<i64>().ok());
                let Some(seconds) = seconds else {
                    message.reply_html(ctx.tr("common.no_args")).await?;
                    return Ok(());
                };
                ctx.db.set("HerokuBackupMod", "backup_period", seconds);
                message
                    .reply_html(ctx.tf("backup.period_set", &[&seconds.to_string()]))
                    .await?;
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
        let mut archive = serde_json::Map::new();
        archive.insert(
            "modules".to_owned(),
            Value::Object(
                modules
                    .iter()
                    .map(|(name, content)| (name.clone(), Value::String(content.clone())))
                    .collect(),
            ),
        );
        archive.insert(
            "primary_repo".to_owned(),
            Value::String(ctx.db.get_str("LoaderMod", "MODULES_REPO", "")),
        );
        archive.insert(
            "repos".to_owned(),
            Value::Array(
                ctx.db
                    .get_string_list("LoaderMod", "ADDITIONAL_REPOS")
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        );
        archive.insert(
            "basic_auth".to_owned(),
            Value::String(ctx.db.get_str("LoaderMod", "basic_auth", "")),
        );

        let dir = PathBuf::from(&ctx.data_root).join("backups");
        std::fs::create_dir_all(&dir)?;
        let name = format!("mods-{}.json", chrono::Local::now().format("%Y%m%d-%H%M%S"));
        let path = dir.join(name);
        crate::config::write_private(
            &path,
            serde_json::to_string_pretty(&Value::Object(archive))?.as_bytes(),
        )?;

        let count = modules.len().to_string();
        let caption = ctx.tf("backup.modules_backup", &[&count]);
        match send_backup_to_channel(ctx, &path, caption.clone()).await {
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
        let mut entries = vec![("db.json".to_owned(), ctx.db.dump_pretty().into_bytes())];
        for (name, content) in &modules {
            entries.push((format!("modules/{name}"), content.as_bytes().to_vec()));
        }
        entries.push((
            "loader.json".to_owned(),
            serde_json::to_vec_pretty(&loader_config(ctx))?,
        ));

        let archive = ziparch::write_zip(&entries);
        let dir = PathBuf::from(&ctx.data_root).join("backups");
        std::fs::create_dir_all(&dir)?;
        let name = format!(
            "heroku-{}.backup",
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let path = dir.join(name);
        crate::config::write_private(&path, &archive)?;

        let prefix = ctx.prefixes(ctx.me_id).first().cloned().unwrap_or_default();
        let caption = ctx.tf("backup.channel_all_caption", &[&escape_html(&prefix)]);
        match send_backup_to_channel(ctx, &path, caption).await {
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
            .join(format!("restore-mods-{}.json", message.id()));
        message
            .reply_html(ctx.tr("backup.restoring_modules"))
            .await?;
        reply
            .download_media(&path)
            .await
            .context("failed to download the modules archive")?;
        let raw = tokio::fs::read_to_string(&path)
            .await
            .context("failed to read the modules archive")?;
        let _ = tokio::fs::remove_file(&path).await;
        let value: Value = serde_json::from_str(&raw).context("invalid modules archive")?;

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

        message
            .reply_html(ctx.tf("backup.mods_restored", &[&restored.to_string()]))
            .await?;
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
            let mut entries = ziparch::read_zip(&raw).context("invalid backup archive")?;
            let db = entries
                .remove("db.json")
                .or_else(|| entries.remove("database.json"));
            let loader = entries.remove("loader.json");
            let nested = entries.remove("mods.zip");

            let mut modules = BTreeMap::new();
            if let Some(nested) = nested {
                match ziparch::read_zip(&nested) {
                    Ok(files) => {
                        for (name, content) in files {
                            if name == "db_mods.json" || name.ends_with('/') {
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
            for (name, content) in entries {
                let Some(name) = name.strip_prefix("modules/") else {
                    continue;
                };
                let Some(name) = PathBuf::from(name)
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
                else {
                    continue;
                };
                modules.insert(name, String::from_utf8_lossy(&content).into_owned());
            }

            let mut restored_db = false;
            if let Some(db) = db {
                match serde_json::from_slice::<Value>(&db) {
                    Ok(value) if value.is_object() => {
                        ctx.db.restore(sanitize_db(value));
                        restored_db = true;
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
            let restored_modules = restore_module_files(ctx, &modules)?;

            if !restored_db && restored_modules == 0 {
                message.reply_html(ctx.tr("backup.bad_file")).await?;
                return Ok(());
            }

            message
                .reply_html(ctx.tf(
                    "backup.all_restored",
                    &[
                        if restored_db { "yes" } else { "no" },
                        &restored_modules.to_string(),
                    ],
                ))
                .await?;
            if restored_db {
                schedule_restart();
            }
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
            return Ok(());
        }

        ctx.db.restore(sanitize_db(value));
        message.reply_html(ctx.tr("backup.restored")).await?;
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
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            modules.insert(name.to_owned(), raw);
        }
    }
    modules
}

fn loader_config(ctx: &Arc<Ctx>) -> Value {
    let mut object = serde_json::Map::new();
    object.insert(
        "primary_repo".to_owned(),
        Value::String(ctx.db.get_str("LoaderMod", "MODULES_REPO", "")),
    );
    object.insert(
        "repos".to_owned(),
        Value::Array(
            ctx.db
                .get_string_list("LoaderMod", "ADDITIONAL_REPOS")
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    object.insert(
        "basic_auth".to_owned(),
        Value::String(ctx.db.get_str("LoaderMod", "basic_auth", "")),
    );
    Value::Object(object)
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

async fn send_backup_to_channel(
    ctx: &Arc<Ctx>,
    path: &std::path::Path,
    caption: String,
) -> Result<Option<String>> {
    let channel = crate::content::ensure_content_channel(ctx).await?;
    let topic = crate::content::ensure_topic(
        ctx,
        crate::content::TOPIC_BACKUPS,
        crate::content::TOPIC_BACKUPS_DESCRIPTION,
    )
    .await?;

    let uploaded = ctx
        .client
        .upload_file(path)
        .await
        .with_context(|| format!("failed to upload {}", path.display()))?;
    let message = grammers_client::message::InputMessage::new()
        .html(caption)
        .document(uploaded)
        .reply_to(Some(topic));
    let sent = ctx
        .client
        .send_message(channel, message)
        .await
        .context("failed to send the backup to the content channel")?;

    let channel_id = channel.id.bare_id_unchecked();
    Ok(Some(format!(
        "https://t.me/c/{channel_id}/{topic}/{}",
        sent.id()
    )))
}

fn create_backup(ctx: &Arc<Ctx>) -> Result<PathBuf> {
    let dir = PathBuf::from(&ctx.data_root).join("backups");
    std::fs::create_dir_all(&dir)?;
    let name = format!(
        "heroku-{}.json",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let path = dir.join(name);
    let raw = ctx.db.dump_pretty();
    crate::config::write_private(&path, raw.as_bytes())?;
    Ok(path)
}

inventory::submit! {
    ModuleFactory(|| Box::new(HerokuBackupMod))
}
