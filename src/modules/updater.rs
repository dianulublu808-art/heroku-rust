// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Updater commands: git status, pull, changelog, rollback, restart and the
//! background update poller.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use grammers_client::message::Button;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, ConfigDefault, ConfigKind, ConfigSpec, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

const DEFAULT_ORIGIN: &str = "https://github.com/dianulublu808-art/heroku-rust";
const FETCH_INTERVAL: Duration = Duration::from_secs(60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

static UPDATING: AtomicBool = AtomicBool::new(false);

pub struct UpdaterMod;

#[async_trait]
impl Module for UpdaterMod {
    fn name(&self) -> &'static str {
        "UpdaterMod"
    }

    fn description(&self) -> &'static str {
        "Update and restart the userbot"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("update", "Fetch and install the latest version").usage("[-f]"),
            CommandDef::new("restart", "Restart the userbot").usage("[-f]"),
            CommandDef::new("autoupdate", "Toggle automatic update checks").usage("[on|off]"),
            CommandDef::new("source", "Show the source repository"),
            CommandDef::new("changelog", "Show recent commits"),
            CommandDef::new("rollback", "Reset to an older commit").usage("<hash|number> [-b]"),
            CommandDef::new("ubstop", "Stop the userbot"),
        ]
    }

    fn config_spec(&self) -> Vec<ConfigSpec> {
        vec![
            ConfigSpec::new(
                "autoupdate",
                "Automatically check for updates",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "disable_notifications",
                "Disable update notifications when autoupdate is off",
                ConfigDefault::Bool(false),
                ConfigKind::Bool,
            ),
            ConfigSpec::new(
                "GIT_ORIGIN_URL",
                "Git repository URL",
                ConfigDefault::Text(DEFAULT_ORIGIN),
                ConfigKind::Text,
            ),
        ]
    }

    async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
        let task_ctx = ctx.clone();
        ctx.spawn_module_task(self.name(), async move {
            let mut ticker = tokio::time::interval(FETCH_INTERVAL);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let autoupdate = task_ctx.db.get_bool("UpdaterMod", "autoupdate", false);
                let Some(root) = repo_root() else {
                    continue;
                };
                match update_status(&root).await {
                    Ok(status) if status.behind > 0 => {
                        let last = task_ctx.db.get_str("UpdaterMod", "last_notified", "");
                        if last == status.remote {
                            continue;
                        }

                        // Python notifies about new versions even when
                        // autoupdate is disabled, unless the owner silenced
                        // those notifications.
                        if !autoupdate {
                            if task_ctx
                                .db
                                .get_bool("UpdaterMod", "disable_notifications", false)
                            {
                                continue;
                            }
                            task_ctx
                                .db
                                .set("UpdaterMod", "last_notified", status.remote.clone());
                            let commits = format_commits(task_ctx.as_ref(), &status);
                            let caption = task_ctx.tf(
                                "updater.update_required",
                                &[&status.remote, &status.remote, &commits],
                            );
                            let buttons = vec![vec![
                                Button::data(
                                    task_ctx.tr("updater.btn_update"),
                                    b"heroku:upd:apply".to_vec(),
                                ),
                                Button::data(
                                    task_ctx.tr("updater.btn_ignore"),
                                    b"heroku:upd:ignore".to_vec(),
                                ),
                            ]];
                            let _ = notify_update(task_ctx.as_ref(), caption, buttons).await;
                            continue;
                        }

                        task_ctx
                            .db
                            .set("UpdaterMod", "last_notified", status.remote.clone());

                        // Python applies minor updates automatically and only
                        // asks the owner to update when the major version
                        // changes. `cargo build` is expensive, so only do it
                        // when the change is safe.
                        let current_major = env!("CARGO_PKG_VERSION")
                            .split('.')
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        let remote_major = remote_version(&root, &status.branch).await;
                        let minor_update = remote_major.as_deref().map(|version| {
                            version.split('.').next() == Some(current_major.as_str())
                        });

                        if minor_update != Some(true) {
                            let _ = notify_update(
                                task_ctx.as_ref(),
                                task_ctx.tf(
                                    "updater.major_update",
                                    &[&status.behind.to_string(), &status.remote],
                                ),
                                vec![vec![Button::data(
                                    task_ctx.tr("updater.btn_update"),
                                    b"heroku:upd:apply".to_vec(),
                                )]],
                            )
                            .await;
                            continue;
                        }

                        let _ = task_ctx
                            .send_self(
                                task_ctx.tf("updater.autoupdate_applying", &[&status.remote]),
                            )
                            .await;

                        if UPDATING.swap(true, Ordering::SeqCst) {
                            continue;
                        }
                        let task_ctx = task_ctx.clone();
                        let task_root = root.clone();
                        tokio::spawn(async move {
                            let result = apply_update(&task_ctx, &task_root).await;
                            UPDATING.store(false, Ordering::SeqCst);
                            match result {
                                Ok(()) => {
                                    let _ =
                                        task_ctx.send_self(task_ctx.tr("updater.updated")).await;
                                    tokio::time::sleep(Duration::from_millis(800)).await;
                                    if let Err(err) = crate::inline::restart_process() {
                                        tracing::error!(
                                            "Restart after the automatic update failed: {err}"
                                        );
                                    }
                                }
                                Err(err) => {
                                    let _ = task_ctx
                                        .send_self(task_ctx.tf(
                                            "updater.update_failed",
                                            &[&escape_html(&format!("{err:#}"))],
                                        ))
                                        .await;
                                }
                            }
                        });
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::debug!("Autoupdate check failed: {err:#}");
                    }
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
            "source" => {
                let origin = ctx
                    .db
                    .get_str("UpdaterMod", "GIT_ORIGIN_URL", DEFAULT_ORIGIN);
                message
                    .reply_html(ctx.tf("updater.source", &[&escape_html(&origin)]))
                    .await?;
            }
            "autoupdate" => {
                let current = ctx.db.get_bool("UpdaterMod", "autoupdate", false);
                let value = args
                    .get(0)
                    .and_then(super::common::parse_bool)
                    .unwrap_or(!current);
                ctx.db.set("UpdaterMod", "autoupdate", value);
                message
                    .reply_html(if value {
                        ctx.tr("updater.autoupdate_on")
                    } else {
                        ctx.tf("updater.autoupdate_off", &[&ctx.prefixes(ctx.me_id)[0]])
                    })
                    .await?;
            }
            "update" => self.update(ctx, message, args).await?,
            "changelog" => self.changelog(ctx, message).await?,
            "rollback" => self.rollback(ctx, message, args).await?,
            "restart" => {
                if !args.flag("f") && !args.flag("force") && confirm_card(ctx, "restart").await {
                    message.reply_html(ctx.tr("updater.confirm_sent")).await?;
                    return Ok(());
                }
                message.reply_html(ctx.tr("updater.restarting")).await?;
                ctx.db.save();
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    if let Err(err) = crate::inline::restart_process() {
                        tracing::error!("Process restart failed: {err}");
                        std::process::exit(0);
                    }
                });
            }
            "ubstop" => {
                if !args.flag("f") && !args.flag("force") && confirm_card(ctx, "stop").await {
                    message.reply_html(ctx.tr("updater.confirm_sent")).await?;
                    return Ok(());
                }
                message.reply_html(ctx.tr("updater.stopped")).await?;
                ctx.db.save();
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    std::process::exit(0);
                });
            }
            _ => {}
        }
        Ok(())
    }
}

impl UpdaterMod {
    async fn update(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(root) = repo_root() else {
            message.reply_html(ctx.tr("updater.no_repo")).await?;
            return Ok(());
        };
        let force = args.flag("f") || args.flag("force");

        message.reply_html(ctx.tr("updater.fetching")).await?;
        let status = match update_status(&root).await {
            Ok(status) => status,
            Err(err) => {
                message
                    .reply_html(ctx.tf(
                        "updater.update_failed",
                        &[&escape_html(&format!("{err:#}"))],
                    ))
                    .await?;
                return Ok(());
            }
        };

        if status.behind == 0 {
            message.reply_html(ctx.tr("updater.up_to_date")).await?;
            return Ok(());
        }

        let commits = format_commits(ctx, &status);
        if !force {
            let html = format!("{commits}\n\n{}", ctx.tr("updater.confirm_inline"));
            let sent = ctx
                .inline
                .send_to_user(
                    ctx,
                    ctx.me_id,
                    html,
                    vec![vec![
                        Button::data(ctx.tr("updater.btn_update"), b"heroku:upd:apply".to_vec()),
                        Button::data(ctx.tr("updater.btn_cancel"), b"heroku:close".to_vec()),
                    ]],
                )
                .await
                .is_ok();
            if sent {
                message.reply_html(ctx.tr("updater.confirm_sent")).await?;
            } else {
                message
                    .reply_html(format!(
                        "{commits}\n\n{}",
                        ctx.tf("updater.confirm", &[&ctx.prefixes(ctx.me_id)[0]])
                    ))
                    .await?;
            }
            return Ok(());
        }

        if UPDATING.swap(true, Ordering::SeqCst) {
            message.reply_html(ctx.tr("updater.busy")).await?;
            return Ok(());
        }

        message
            .reply_html(format!("{commits}\n\n{}", ctx.tr("updater.updating")))
            .await?;

        let ctx = ctx.clone();
        tokio::spawn(async move {
            let result = apply_update(&ctx, &root).await;
            UPDATING.store(false, Ordering::SeqCst);
            match result {
                Ok(()) => {
                    let _ = ctx.send_self(ctx.tr("updater.updated")).await;
                    tokio::time::sleep(Duration::from_millis(800)).await;
                    if let Err(err) = crate::inline::restart_process() {
                        tracing::error!("Restart after update failed: {err}");
                    }
                }
                Err(err) => {
                    let _ = ctx
                        .send_self(ctx.tf(
                            "updater.update_failed",
                            &[&escape_html(&format!("{err:#}"))],
                        ))
                        .await;
                }
            }
        });
        Ok(())
    }

    async fn changelog(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
        let Some(root) = repo_root() else {
            message.reply_html(ctx.tr("updater.no_repo")).await?;
            return Ok(());
        };
        // Python reads the first section of `CHANGELOG.md`; keep that behaviour
        // and fall back to git history when the file is missing.
        if let Ok(raw) = std::fs::read_to_string(root.join("CHANGELOG.md")) {
            if let Some(section) = raw.split("##").nth(1) {
                let section = section.trim();
                if !section.is_empty() {
                    message
                        .reply_html(ctx.tf("updater.changelog", &[&escape_html(section)]))
                        .await?;
                    return Ok(());
                }
            }
        }
        let log = git(&root, &["log", "--pretty=format:%h%x09%s", "-n", "20"]).await?;
        let mut lines = vec![ctx.tr("updater.changelog_title")];
        for line in log.lines() {
            if let Some((hash, subject)) = line.split_once('\t') {
                lines.push(format!(
                    "▪️ <code>{}</code> {}",
                    escape_html(hash),
                    escape_html(subject)
                ));
            }
        }
        message.reply_html(lines.join("\n")).await?;
        Ok(())
    }

    async fn rollback(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
        let Some(root) = repo_root() else {
            message.reply_html(ctx.tr("updater.no_repo")).await?;
            return Ok(());
        };

        let log = git(
            &root,
            &["log", "--pretty=format:%H%x09%h%x09%s", "-n", "10"],
        )
        .await?;
        let entries: Vec<(&str, &str, &str)> = log
            .lines()
            .filter_map(|line| {
                let mut parts = line.splitn(3, '\t');
                Some((parts.next()?, parts.next()?, parts.next()?))
            })
            .collect();

        let Some(selector) = args.get(0) else {
            let mut lines = vec![ctx.tr("updater.rollback_title")];
            for (index, (_, short, subject)) in entries.iter().enumerate() {
                lines.push(format!(
                    "<code>{}</code>. <code>{}</code> {}",
                    index + 1,
                    escape_html(short),
                    escape_html(subject)
                ));
            }
            lines.push(String::new());
            lines.push(ctx.tr("updater.rollback_inline"));

            let mut rows = Vec::new();
            for (hash, short, subject) in entries.iter().take(6) {
                rows.push(vec![
                    Button::data(
                        format!("↩️ {} {}", short, truncate(subject, 28)),
                        format!("heroku:upd:rb:{hash}").into_bytes(),
                    ),
                    Button::data("🔨", format!("heroku:upd:rbb:{hash}").into_bytes()),
                ]);
            }
            rows.push(vec![Button::data(
                ctx.tr("updater.btn_cancel"),
                b"heroku:close".to_vec(),
            )]);

            let sent = ctx
                .inline
                .send_to_user(ctx, ctx.me_id, lines.join("\n"), rows)
                .await
                .is_ok();
            if !sent {
                message
                    .reply_html(ctx.tf("updater.rollback_usage", &[&ctx.prefixes(ctx.me_id)[0]]))
                    .await?;
            } else {
                message.reply_html(ctx.tr("updater.confirm_sent")).await?;
            }
            return Ok(());
        };

        let target = if let Ok(index) = selector.parse::<usize>() {
            entries
                .get(index.saturating_sub(1))
                .map(|(hash, _, _)| (*hash).to_owned())
                .with_context(|| format!("no commit at position {index}"))?
        } else {
            selector.to_owned()
        };

        let reset = git(&root, &["reset", "--hard", &target]).await;
        if let Err(err) = reset {
            message
                .reply_html(ctx.tf(
                    "updater.update_failed",
                    &[&escape_html(&format!("{err:#}"))],
                ))
                .await?;
            return Ok(());
        }

        if args.flag("b") || args.flag("build") {
            message.reply_html(ctx.tr("updater.building")).await?;
            build_release(&root).await?;
            message.reply_html(ctx.tr("updater.rolled_back")).await?;
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(800)).await;
                if let Err(err) = crate::inline::restart_process() {
                    tracing::error!("Restart after rollback failed: {err}");
                }
            });
        } else {
            message
                .reply_html(ctx.tf("updater.rolled_back_hint", &[&ctx.prefixes(ctx.me_id)[0]]))
                .await?;
        }
        Ok(())
    }
}

struct UpdateStatus {
    behind: usize,
    remote: String,
    branch: String,
    commits: Vec<String>,
}

async fn update_status(root: &Path) -> Result<UpdateStatus> {
    git(root, &["fetch", "--all", "--prune"]).await?;

    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "master".to_owned());
    let branch = branch.trim().to_owned();
    let upstream = git(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )
    .await
    .unwrap_or_else(|_| format!("origin/{branch}"));

    let behind = git(root, &["rev-list", "--count", &format!("HEAD..{upstream}")])
        .await?
        .parse::<usize>()
        .unwrap_or(0);
    let remote = git(root, &["rev-parse", "--short", &upstream])
        .await
        .unwrap_or_default();
    let log = git(
        root,
        &[
            "log",
            "--pretty=format:%h%x09%s",
            "-n",
            "10",
            &format!("HEAD..{upstream}"),
        ],
    )
    .await
    .unwrap_or_default();
    let commits = log.lines().map(ToOwned::to_owned).collect();

    Ok(UpdateStatus {
        behind,
        remote,
        branch,
        commits,
    })
}

/// Read the `version` field of `Cargo.toml` from the fetched remote branch.
async fn remote_version(root: &Path, branch: &str) -> Option<String> {
    let remote = git(root, &["show", &format!("origin/{branch}:Cargo.toml")])
        .await
        .ok()?;
    let mut in_package = false;
    for line in remote.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("version") {
                let value = value.trim_start_matches([' ', '=']).trim();
                let value = value.trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

async fn apply_update(ctx: &Arc<Ctx>, root: &Path) -> Result<()> {
    git(root, &["pull", "--ff-only"]).await?;
    if root.join("Cargo.toml").exists() {
        let _ = ctx.send_self(ctx.tr("updater.building")).await;
        build_release(root).await?;
    }
    Ok(())
}

pub(crate) async fn build_release(root: &Path) -> Result<()> {
    let output = tokio::time::timeout(
        BUILD_TIMEOUT,
        tokio::process::Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(root)
            .output(),
    )
    .await
    .context("cargo build timed out")?
    .context("failed to start cargo (is Rust installed?)")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: String = stderr.lines().rev().take(12).collect::<Vec<_>>().join("\n");
        bail!("cargo build --release failed:\n{tail}");
    }
    Ok(())
}

pub(crate) async fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", "echo")
            .output(),
    )
    .await
    .context("git command timed out")?
    .context("failed to start git")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn repo_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("HEROKU_REPO") {
        let path = PathBuf::from(root);
        if path.join(".git").exists() {
            return Some(path);
        }
    }

    let mut starts = Vec::new();
    if let Ok(current) = std::env::current_dir() {
        starts.push(current);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }

    for start in starts {
        for directory in start.ancestors() {
            if directory.join(".git").exists() {
                return Some(directory.to_path_buf());
            }
        }
    }
    None
}

const UPDATE_PHOTO: &str =
    "https://raw.githubusercontent.com/Heroku-userbot/assets/refs/heads/main/updated.jpg";

/// Send the update card with the banner used by Python, falling back to a
/// plain saved-messages message when the photo cannot be sent.
async fn notify_update(ctx: &Ctx, caption: String, buttons: Vec<Vec<Button>>) -> Result<()> {
    if ctx
        .send_self_photo(UPDATE_PHOTO, caption.clone(), buttons)
        .await
        .is_err()
    {
        ctx.send_self(caption).await?;
    }
    Ok(())
}

fn format_commits(ctx: &Ctx, status: &UpdateStatus) -> String {
    let mut lines = vec![ctx.tf(
        "updater.available",
        &[
            &status.behind.to_string(),
            &escape_html(&status.branch),
            &escape_html(&status.remote),
        ],
    )];
    for commit in status.commits.iter().take(10) {
        if let Some((hash, subject)) = commit.split_once('\t') {
            lines.push(format!(
                "▪️ <code>{}</code> {}",
                escape_html(hash),
                escape_html(subject)
            ));
        }
    }
    lines.join("\n")
}

/// Ask for confirmation through the inline bot. Returns `false` (and lets the
/// caller fall back to a text prompt) when the bot is not running or the owner
/// has not started it.
async fn confirm_card(ctx: &Arc<Ctx>, action: &str) -> bool {
    let (text_key, callback) = match action {
        "restart" => ("updater.confirm_restart", b"heroku:upd:restart".to_vec()),
        "stop" => ("updater.confirm_stop", b"heroku:upd:stop".to_vec()),
        _ => return false,
    };
    ctx.inline
        .send_to_user(
            ctx,
            ctx.me_id,
            ctx.tr(text_key).to_owned(),
            vec![vec![
                Button::data(ctx.tr("updater.btn_confirm"), callback),
                Button::data(ctx.tr("updater.btn_cancel"), b"heroku:close".to_vec()),
            ]],
        )
        .await
        .is_ok()
}

/// Perform the complete update flow from an inline confirmation card.
pub(crate) async fn inline_apply_update(ctx: Arc<Ctx>) {
    let Some(root) = repo_root() else {
        let _ = ctx.send_self(ctx.tr("updater.no_repo")).await;
        return;
    };
    if UPDATING.swap(true, Ordering::SeqCst) {
        let _ = ctx.send_self(ctx.tr("updater.busy")).await;
        return;
    }

    let _ = ctx.send_self(ctx.tr("updater.fetching")).await;
    let result = async {
        let status = update_status(&root).await?;
        if status.behind == 0 {
            return Ok::<bool, anyhow::Error>(false);
        }
        let commits = format_commits(&ctx, &status);
        let _ = ctx
            .send_self(format!("{commits}\n\n{}", ctx.tr("updater.updating")))
            .await;
        apply_update(&ctx, &root).await?;
        Ok(true)
    }
    .await;

    UPDATING.store(false, Ordering::SeqCst);
    match result {
        Ok(true) => {
            let _ = ctx.send_self(ctx.tr("updater.updated")).await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(err) = crate::inline::restart_process() {
                tracing::error!("Restart after inline update failed: {err}");
            }
        }
        Ok(false) => {
            let _ = ctx.send_self(ctx.tr("updater.up_to_date")).await;
        }
        Err(err) => {
            let _ = ctx
                .send_self(ctx.tf(
                    "updater.update_failed",
                    &[&escape_html(&format!("{err:#}"))],
                ))
                .await;
        }
    }
}

/// Reset the repository to a commit picked from the inline rollback card.
pub(crate) async fn rollback_from_inline(ctx: &Arc<Ctx>, target: &str, build: bool) -> Result<()> {
    let Some(root) = repo_root() else {
        ctx.send_self(ctx.tr("updater.no_repo")).await?;
        return Ok(());
    };
    git(&root, &["reset", "--hard", target]).await?;
    if build {
        let _ = ctx.send_self(ctx.tr("updater.building")).await;
        build_release(&root).await?;
        let _ = ctx.send_self(ctx.tr("updater.rolled_back")).await;
        tokio::spawn(async {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(err) = crate::inline::restart_process() {
                tracing::error!("Restart after rollback failed: {err}");
            }
        });
    } else {
        let _ = ctx
            .send_self(ctx.tf("updater.rolled_back_hint", &[&ctx.prefixes(ctx.me_id)[0]]))
            .await;
    }
    Ok(())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

inventory::submit! {
    ModuleFactory(|| Box::new(UpdaterMod))
}
