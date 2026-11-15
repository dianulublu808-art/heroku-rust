// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Heroku Userbot — Telegram MTProto userbot written in Rust.

// Some helpers (formatting, peer helpers, security lists) are part of the
// module-facing API and are used by optional modules or upcoming features.
// Keep them compiled without flooding the build with dead-code warnings.
#![allow(dead_code)]

mod accounts;
mod args;
mod config;
mod content;
mod ctx;
mod database;
mod dispatcher;
mod inline;
mod logbuf;
mod login;
mod module;
mod modules;
mod protection;
mod security;
mod tg;
mod translations;
mod ziparch;

use std::sync::Arc;

use anyhow::{Context, Result};
use grammers_client::update::Update;
use grammers_client::Client;
use grammers_mtsender::{SenderPool, UpdatesConfiguration};
use grammers_session::storages::{erase, SqliteSession};
use tokio::task::JoinSet;
use tracing::{error, info, warn};
use tracing_subscriber::filter::EnvFilter;

use crate::config::{Cli, Config};
use crate::ctx::Ctx;
use crate::database::Database;
use crate::dispatcher::Dispatcher;
use crate::inline::InlineManager;
use crate::logbuf::{LogBuffer, TeeMaker};
use crate::module::Modules;
use crate::security::Security;
use crate::tg::Msg;
use crate::translations::Translator;

async fn async_main() -> Result<()> {
    let cli = Cli::parse();
    if cli.version {
        println!("{} {}", config::APP_NAME, config::VERSION);
        return Ok(());
    }

    let data_root = config::resolve_data_root(&cli);
    if cli.wipe {
        config::wipe(&data_root)?;
        return Ok(());
    }

    print_banner(&data_root);

    let config = Config::load_or_setup(&data_root)?;
    let log_buffer = LogBuffer::default();
    init_tracing(&config, log_buffer.clone());

    info!(
        "Starting {} {} (data root: {})",
        config::APP_NAME,
        config::VERSION,
        data_root.display()
    );

    let (db_path, session_path) = accounts::active_paths(&data_root);
    let db = Arc::new(Database::open(db_path)?);
    let security = Security::new(db.clone());

    let protection = protection::protection();
    protection.configure(
        db.get_bool("APIRatelimiterMod", "enabled", true),
        db.get_i64("APIRatelimiterMod", "threshold", 100)
            .clamp(1, 1_000_000) as usize,
        db.get_i64("APIRatelimiterMod", "time_sample", 15)
            .clamp(1, 86_400) as u64,
        db.get_i64("APIRatelimiterMod", "local_floodwait", 30)
            .clamp(1, 86_400) as u64,
    );

    let session = Arc::new(
        SqliteSession::open(&session_path)
            .await
            .context("failed to open session database")?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&session_path, std::fs::Permissions::from_mode(0o600));
    }
    let session_view = erase(session.clone());

    let SenderPool {
        runner,
        updates,
        handle,
    } = SenderPool::new(session.clone(), config.api_id);
    let client = Client::new(handle.clone());
    let pool_task = tokio::spawn(runner.run());

    if !client.is_authorized().await? {
        let qr = cli.qr
            || std::env::var("TG_QR")
                .map(|value| {
                    let value = value.trim().to_ascii_lowercase();
                    !value.is_empty() && value != "0" && value != "false" && value != "no"
                })
                .unwrap_or(false);
        if qr {
            login::qr_login(&client, &session_view, &config).await?;
        } else {
            login::interactive_login(&client, &session_view, &config).await?;
        }
    }

    let me = client.get_me().await.context("failed to fetch self user")?;
    let me_id = me.id().bot_api_dialog_id_unchecked();
    let me_username = me.username().map(ToOwned::to_owned);
    let me_name = me.full_name();
    tg::set_self_id(me_id);
    security.ensure_owner(me_id);

    info!(
        "Logged in as {} (@{})",
        me_name,
        me_username.as_deref().unwrap_or("no username")
    );

    let lang = db.get_str("heroku.translations", "lang", &config.lang);
    let translator = Translator::new(lang);
    let external_langs = db.get_string_list("heroku.translations", "external");
    if !external_langs.is_empty() {
        crate::modules::translations::load_external_packs(&translator, &external_langs).await;
    }

    let modules = Modules::discover();
    info!("Discovered {} modules", modules.all().len());

    let inline = Arc::new(InlineManager::new());
    let ctx = Arc::new(Ctx {
        client: client.clone(),
        session: session_view.clone(),
        db: db.clone(),
        config: config.clone(),
        data_root: data_root.clone(),
        modules,
        security,
        translator,
        log_buffer,
        inline: inline.clone(),
        started: std::time::Instant::now(),
        me_id,
        me_username,
        me_name,
    });

    for module in ctx.modules.all() {
        for spec in module.config_spec() {
            if ctx.db.get_value(module.name(), spec.key).is_none() {
                ctx.db
                    .set_value(module.name(), spec.key, spec.default.to_value());
            }
        }
    }

    // Start the bot before `on_load` so modules (Quickstart) can use it for
    // their first-run cards, exactly like Python's `inline_ready` hook.
    if let Err(err) = ctx.inline.start(&ctx).await {
        warn!("Inline bot is not running: {err:#}");
    }

    for module in ctx.modules.all() {
        match module.on_load(&ctx).await {
            Ok(()) => tracing::debug!("Loaded module {}", module.name()),
            Err(err) => error!("Failed to load module {}: {err:#}", module.name()),
        }
    }

    warm_up_dialogs(client.clone());

    let dispatcher = Arc::new(Dispatcher::new(&ctx));
    let mut updates = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: false })
        .await
        .map_err(|err| anyhow::anyhow!("failed to start update stream: {err}"))?;

    info!("Heroku is running. Press Ctrl+C to stop.");
    let _ = ctx
        .send_self(format!(
            "🚀 <b>{} {}</b> started as <b>{}</b>",
            config::APP_NAME,
            config::VERSION,
            tg::escape_html(&ctx.me_name)
        ))
        .await;

    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown requested");
                break;
            }
            _ = tick.tick() => {
                dispatcher.tick_ratelimit();
                sync_protection(&ctx.db);
                if protection.take_warning() {
                    let freeze = protection.freeze_secs().to_string();
                    let pending = protection.pending_calls().to_string();
                    let _ = ctx
                        .send_self(ctx.tf(
                            "api_protection.warning",
                            &[&freeze, &pending],
                        ))
                        .await;
                }
            }
            update = updates.next() => {
                match update {
                    Ok(Update::NewMessage(message)) => {
                        let ctx = ctx.clone();
                        let dispatcher = dispatcher.clone();
                        let message = Msg::new(message.into_inner());
                        tasks.spawn(async move {
                            dispatcher.handle(&ctx, message).await;
                        });
                    }
                    Ok(Update::MessageEdited(message)) => {
                        let ctx = ctx.clone();
                        let dispatcher = dispatcher.clone();
                        let message = Msg::new(message.into_inner());
                        tasks.spawn(async move {
                            dispatcher.handle_edit(&ctx, message).await;
                        });
                    }
                    Ok(_) => {}
                    Err(err) => warn!("Update stream error: {err:#}"),
                }
            }
            Some(result) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(err) = result {
                    error!("Message handler task panicked: {err}");
                }
            }
        }
    }

    for module in ctx.modules.all() {
        if let Err(err) = module.on_unload(&ctx).await {
            error!("Failed to unload module {}: {err:#}", module.name());
        }
    }

    ctx.inline.stop().await;

    if let Err(err) = updates.sync_update_state().await {
        warn!("Failed to persist update state: {err}");
    }
    handle.quit();
    let _ = pool_task.await;
    db.save();
    info!("Session saved. Bye!");
    Ok(())
}

fn sync_protection(db: &Database) {
    protection::protection().configure(
        db.get_bool("APIRatelimiterMod", "enabled", true),
        db.get_i64("APIRatelimiterMod", "threshold", 100)
            .clamp(1, 1_000_000) as usize,
        db.get_i64("APIRatelimiterMod", "time_sample", 15)
            .clamp(1, 86_400) as u64,
        db.get_i64("APIRatelimiterMod", "local_floodwait", 30)
            .clamp(1, 86_400) as u64,
    );
}

fn print_banner(data_root: &std::path::Path) {
    use std::io::IsTerminal;

    if !std::io::stdout().is_terminal() {
        return;
    }

    const PURPLE: &str = "\x1b[38;2;165;110;245m";
    const DIM: &str = "\x1b[2m";
    const BOLD: &str = "\x1b[1m";
    const RESET: &str = "\x1b[0m";

    println!();
    println!(
        "{PURPLE}{BOLD}  🪐  HEROKU{RESET} {DIM}v{}{RESET}",
        config::VERSION
    );
    println!("{DIM}      Telegram userbot · Rust edition{RESET}");
    println!("{DIM}      data: {}{RESET}", data_root.display());
    println!();
}

fn init_tracing(config: &Config, log_buffer: LogBuffer) {
    let filter = EnvFilter::try_new(&config.log_level)
        .or_else(|_| EnvFilter::try_new("info"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(TeeMaker(log_buffer))
        .try_init();
}

/// Fetch dialogs once so that the session cache contains access hashes. Without
/// them Telegram cannot resolve some updates after a restart.
fn warm_up_dialogs(client: Client) {
    tokio::spawn(async move {
        let mut dialogs = client.iter_dialogs();
        let mut count = 0usize;
        loop {
            match dialogs.next().await {
                Ok(Some(_)) => count += 1,
                Ok(None) => break,
                Err(err) => {
                    warn!("Dialog cache warm-up failed: {err}");
                    return;
                }
            }
        }
        tracing::debug!("Cached {count} dialogs");
    });
}

fn main() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to create the Tokio runtime")?;
    runtime.block_on(async_main())
}
