// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Database backups and restores.

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
            CommandDef::new(
                "backupmods",
                "Back up modules (not available in Rust build)",
            ),
            CommandDef::new(
                "restoremods",
                "Restore modules (not available in Rust build)",
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
                    Ok(path) => match ctx.client.upload_file(&path).await {
                        Ok(uploaded) => {
                            let peer =
                                grammers_session::types::PeerId::self_user().to_ambient_ref();
                            let message = grammers_client::message::InputMessage::new()
                                .html("💾 <b>Automatic backup</b>")
                                .document(uploaded);
                            if let Err(err) = ctx.client.send_message(peer, message).await {
                                tracing::error!("Failed to send automatic backup: {err}");
                            }
                        }
                        Err(err) => {
                            tracing::error!("Failed to upload automatic backup: {err}")
                        }
                    },
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
            "backupdb" | "backupall" => {
                message.reply_html(ctx.tr("backup.creating")).await?;
                let path = create_backup(ctx)?;
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
            "restoredb" | "restoreall" => self.restore(ctx, message).await?,
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
                message.reply_html(ctx.tr("loader.unavailable")).await?;
            }
        }
        Ok(())
    }
}

impl HerokuBackupMod {
    async fn restore(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
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
            .join(format!("restore-{}.json", message.id()));
        message.reply_html(ctx.tr("backup.restoring")).await?;
        reply
            .download_media(&path)
            .await
            .context("failed to download the backup")?;
        let raw = tokio::fs::read_to_string(&path)
            .await
            .context("failed to read the backup")?;
        let value: Value = serde_json::from_str(&raw).context("invalid JSON")?;
        let _ = tokio::fs::remove_file(&path).await;
        if !value.is_object() {
            message.reply_html(ctx.tr("backup.bad_file")).await?;
            return Ok(());
        }
        ctx.db.restore(value);
        message.reply_html(ctx.tr("backup.restored")).await?;
        Ok(())
    }
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
