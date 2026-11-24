// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Module lifecycle helpers.
//!
//! Loading is not just `on_load().await`: a hook can fail after it has spawned
//! tasks or registered partial state. This module owns the full transaction —
//! validate declarative watchers, run `on_load`, track success/failure, abort
//! managed tasks on rollback and unload — so the startup path and `.togglemod`
//! behave identically.

use std::sync::Arc;

use anyhow::{anyhow, Result};

use crate::ctx::Ctx;
use crate::module::Module;

/// Reject a module whose declarative watcher regex cannot compile.
///
/// The dispatcher caches invalid patterns as `None` (so it does not retry on
/// every message), but silence is not a lifecycle policy: a broken watcher is a
/// broken module, and the owner must see why it was not loaded.
pub(crate) fn validate_watchers(module: &dyn Module) -> Result<()> {
    let mut errors = Vec::new();
    for watcher in module.watchers() {
        if let Err(err) = watcher.filter.validate_regex() {
            errors.push(format!("watcher `{}`: {err}", watcher.name));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid watcher filter(s) in module {}: {}",
            module.name(),
            errors.join("; ")
        ))
    }
}

/// Load a module and roll back partial work if `on_load` fails.
///
/// On failure the module is marked failed (so dispatch/help/inline skip it),
/// its managed tasks are aborted, and `on_unload` is called as a rollback hook.
/// `on_unload` must therefore be idempotent: it may run on a partially loaded
/// module after a failed `on_load`.
pub(crate) async fn load(ctx: &Arc<Ctx>, module: &Arc<dyn Module>) -> Result<()> {
    let name = module.name();
    if ctx.module_loaded(name) {
        return Ok(());
    }

    if let Err(err) = validate_watchers(module.as_ref()) {
        ctx.mark_module_failed(name, format!("{err:#}"));
        ctx.abort_module_tasks(name);
        tracing::error!("Cannot load module {name}: {err:#}");
        module.on_error(ctx, &format!("on_load {name}"), &err).await;
        return Err(err);
    }

    match module.on_load(ctx).await {
        Ok(()) => {
            ctx.mark_module_loaded(name);
            tracing::debug!("Loaded module {name}");
            Ok(())
        }
        Err(err) => {
            ctx.mark_module_failed(name, format!("{err:#}"));
            let aborted = ctx.abort_module_tasks(name);
            tracing::error!(
                "Failed to load module {name}: {err:#}; aborted {aborted} managed task(s)"
            );

            // Rollback is best effort: the module may have registered part of
            // its state before the error. A rollback failure must not mask the
            // original load error.
            if let Err(rollback) = module.on_unload(ctx).await {
                tracing::error!("Rollback of module {name} failed: {rollback:#}");
            }
            ctx.abort_module_tasks(name);
            module.on_error(ctx, &format!("on_load {name}"), &err).await;
            Err(err)
        }
    }
}

/// Unload a module, abort all remaining managed tasks and forget its state.
///
/// The module is unloaded even if `on_unload` itself fails; otherwise its
/// tasks would keep running and dispatch would continue to call it.
pub(crate) async fn unload(ctx: &Arc<Ctx>, module: &Arc<dyn Module>) {
    let name = module.name();
    if let Err(err) = module.on_unload(ctx).await {
        tracing::error!("Failed to unload module {name}: {err:#}");
        module
            .on_error(ctx, &format!("on_unload {name}"), &err)
            .await;
    }
    let aborted = ctx.abort_module_tasks(name);
    if aborted > 0 {
        tracing::debug!("Aborted {aborted} managed task(s) of module {name}");
    }
    ctx.mark_module_unloaded(name);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use grammers_client::Client;
    use grammers_mtsender::{SenderPool, SenderPoolFatHandle};
    use grammers_session::storages::{erase, SqliteSession};

    use super::{load, unload};
    use crate::args::Args;
    use crate::config::Config;
    use crate::ctx::Ctx;
    use crate::database::Database;
    use crate::inline::InlineManager;
    use crate::logbuf::LogBuffer;
    use crate::module::{Module, Modules, WatcherDef, WatcherFilter};
    use crate::security::Security;
    use crate::tg::Msg;
    use crate::translations::Translator;

    struct Harness {
        ctx: Arc<Ctx>,
        root: PathBuf,
        handle: SenderPoolFatHandle,
        pool: tokio::task::JoinHandle<()>,
    }

    impl Harness {
        async fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "heroku-lifecycle-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            std::fs::create_dir_all(&root).expect("create temp root");

            let session_path = root.join("heroku.session");
            let session = Arc::new(
                SqliteSession::open(&session_path)
                    .await
                    .expect("open test session"),
            );
            let session_view = erase(session.clone());
            let SenderPool { runner, handle, .. } = SenderPool::new(session, 0);
            let pool = tokio::spawn(async move {
                let _ = runner.run().await;
            });
            let client = Client::new(handle.clone());

            let db =
                Arc::new(Database::open(root.join("heroku.json")).expect("open test database"));
            let security = Security::new(db.clone());
            let config = Config {
                api_id: 0,
                api_hash: String::new(),
                phone: None,
                lang: "en".to_owned(),
                log_level: "error".to_owned(),
                session_name: "test".to_owned(),
                inline_bot_token: None,
                inline_bot_username: None,
                proxy: None,
                web: None,
            };
            let ctx = Arc::new(Ctx {
                client,
                session: session_view,
                db,
                config,
                data_root: root.clone(),
                modules: Modules::discover(),
                security,
                translator: Translator::new("en"),
                log_buffer: LogBuffer::default(),
                inline: Arc::new(InlineManager::new()),
                started: std::time::Instant::now(),
                me_id: 42,
                me_username: Some("heroku_test".to_owned()),
                me_name: "Heroku Test".to_owned(),
                lifecycle: Ctx::new_lifecycle(),
                module_tasks: Ctx::new_module_tasks(),
            });

            Self {
                ctx,
                root,
                handle,
                pool,
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.handle.quit();
            self.pool.abort();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    struct FakeModule {
        name: &'static str,
        fail_load: AtomicBool,
        spawn_on_load: bool,
        regex: Option<&'static str>,
        load_calls: AtomicUsize,
        unload_calls: AtomicUsize,
        error_calls: AtomicUsize,
        task_counter: Arc<AtomicUsize>,
    }

    impl FakeModule {
        fn new(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                fail_load: AtomicBool::new(false),
                spawn_on_load: false,
                regex: None,
                load_calls: AtomicUsize::new(0),
                unload_calls: AtomicUsize::new(0),
                error_calls: AtomicUsize::new(0),
                task_counter: Arc::new(AtomicUsize::new(0)),
            })
        }
    }

    #[async_trait]
    impl Module for FakeModule {
        fn name(&self) -> &'static str {
            self.name
        }

        fn watchers(&self) -> Vec<WatcherDef> {
            match self.regex {
                Some(pattern) => vec![WatcherDef::new(
                    "fake",
                    WatcherFilter {
                        regex: Some(pattern.to_owned()),
                        ..Default::default()
                    },
                )],
                None => Vec::new(),
            }
        }

        async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
            self.load_calls.fetch_add(1, Ordering::Relaxed);
            if self.spawn_on_load {
                let counter = self.task_counter.clone();
                ctx.spawn_module_task(self.name, async move {
                    loop {
                        counter.fetch_add(1, Ordering::Relaxed);
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                });
            }
            if self.fail_load.load(Ordering::Relaxed) {
                return Err(anyhow!("fake on_load failure"));
            }
            Ok(())
        }

        async fn on_unload(&self, _ctx: &Arc<Ctx>) -> Result<()> {
            self.unload_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        async fn on_error(&self, _ctx: &Arc<Ctx>, _source: &str, _error: &anyhow::Error) {
            self.error_calls.fetch_add(1, Ordering::Relaxed);
        }

        async fn handle(
            &self,
            _ctx: &Arc<Ctx>,
            _command: &str,
            _message: &Msg,
            _args: &Args,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn load_marks_success_and_unload_aborts_managed_tasks() {
        let harness = Harness::new().await;
        let mut fake = FakeModule::new("LifecycleGoodMod");
        Arc::get_mut(&mut fake)
            .expect("unique module")
            .spawn_on_load = true;
        let module: Arc<dyn Module> = fake.clone();

        load(&harness.ctx, &module).await.expect("load module");
        assert!(harness.ctx.module_loaded("LifecycleGoodMod"));
        assert!(harness.ctx.module_failed("LifecycleGoodMod").is_none());
        assert_eq!(harness.ctx.module_task_count("LifecycleGoodMod"), 1);

        // Loading again must not run the hook twice.
        load(&harness.ctx, &module).await.expect("reload");
        assert_eq!(fake.load_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fake.unload_calls.load(Ordering::Relaxed), 0);

        unload(&harness.ctx, &module).await;
        assert!(!harness.ctx.module_loaded("LifecycleGoodMod"));
        assert!(harness.ctx.module_failed("LifecycleGoodMod").is_none());
        assert_eq!(harness.ctx.module_task_count("LifecycleGoodMod"), 0);
        assert_eq!(fake.unload_calls.load(Ordering::Relaxed), 1);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let frozen = fake.task_counter.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            fake.task_counter.load(Ordering::Relaxed),
            frozen,
            "unload must stop the module's background task"
        );
    }

    #[tokio::test]
    async fn failed_load_rolls_back_tasks_and_calls_unload_and_on_error() {
        let harness = Harness::new().await;
        let mut fake = FakeModule::new("LifecycleBrokenMod");
        {
            let fake = Arc::get_mut(&mut fake).expect("unique module");
            fake.fail_load.store(true, Ordering::Relaxed);
            fake.spawn_on_load = true;
        }
        let module: Arc<dyn Module> = fake.clone();

        let err = load(&harness.ctx, &module)
            .await
            .expect_err("load must fail");
        assert!(format!("{err:#}").contains("fake on_load failure"));

        assert!(!harness.ctx.module_loaded("LifecycleBrokenMod"));
        assert!(harness
            .ctx
            .module_failed("LifecycleBrokenMod")
            .is_some_and(|reason| reason.contains("fake on_load failure")));
        assert_eq!(fake.unload_calls.load(Ordering::Relaxed), 1);
        assert_eq!(fake.error_calls.load(Ordering::Relaxed), 1);
        assert_eq!(harness.ctx.module_task_count("LifecycleBrokenMod"), 0);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let frozen = fake.task_counter.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(
            fake.task_counter.load(Ordering::Relaxed),
            frozen,
            "a failed load must not leave its task running"
        );
    }

    #[tokio::test]
    async fn invalid_watcher_regex_blocks_load_before_on_load() {
        let harness = Harness::new().await;
        let mut fake = FakeModule::new("LifecycleBadRegexMod");
        Arc::get_mut(&mut fake).expect("unique module").regex = Some("([a-z");
        let module: Arc<dyn Module> = fake.clone();

        let err = load(&harness.ctx, &module)
            .await
            .expect_err("invalid watcher must reject the module");
        assert!(format!("{err:#}").contains("invalid watcher filter"));
        assert_eq!(fake.load_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fake.unload_calls.load(Ordering::Relaxed), 0);
        assert_eq!(fake.error_calls.load(Ordering::Relaxed), 1);
        assert!(harness.ctx.module_failed("LifecycleBadRegexMod").is_some());
    }

    #[tokio::test]
    async fn a_later_successful_load_clears_a_previous_failure() {
        let harness = Harness::new().await;
        let mut fake = FakeModule::new("LifecycleRetryMod");
        Arc::get_mut(&mut fake)
            .expect("unique module")
            .fail_load
            .store(true, Ordering::Relaxed);
        let module: Arc<dyn Module> = fake.clone();

        assert!(load(&harness.ctx, &module).await.is_err());
        assert!(harness.ctx.module_failed("LifecycleRetryMod").is_some());

        fake.fail_load.store(false, Ordering::Relaxed);
        load(&harness.ctx, &module).await.expect("retry succeeds");
        assert!(harness.ctx.module_loaded("LifecycleRetryMod"));
        assert!(harness.ctx.module_failed("LifecycleRetryMod").is_none());
        assert_eq!(fake.load_calls.load(Ordering::Relaxed), 2);
    }
}
