// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Heroku Userbot — Telegram MTProto userbot written in Rust.

// Some helpers (formatting, peer helpers, security lists) are part of the
// module-facing API and are used by optional modules or upcoming features.
// Keep them compiled without flooding the build with dead-code warnings.
#![allow(dead_code)]

mod args;
mod config;
mod ctx;
mod database;
mod dispatcher;
mod logbuf;
mod login;
mod module;
mod modules;
mod security;
mod tg;
mod translations;

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

    let db = Arc::new(Database::open(Config::db_path(&data_root))?);
    let security = Security::new(db.clone());

    let session = Arc::new(
        SqliteSession::open(Config::session_path(&data_root))
            .await
            .context("failed to open session database")?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            Config::session_path(&data_root),
            std::fs::Permissions::from_mode(0o600),
        );
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
        login::interactive_login(&client, &session_view, &config).await?;
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

    let modules = Modules::discover();
    info!("Discovered {} modules", modules.all().len());

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

    if let Err(err) = updates.sync_update_state().await {
        warn!("Failed to persist update state: {err}");
    }
    handle.quit();
    let _ = pool_task.await;
    db.save();
    info!("Session saved. Bye!");
    Ok(())
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
