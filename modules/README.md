# External Rust modules

Any file in this directory is compiled into Heroku by `build.rs`. A module is a
normal Rust source file that exposes `pub struct Module` and implements
`crate::module::Module`:

```rust
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module as ModuleApi, OWNER};
use crate::tg::Msg;

pub struct Module;

#[async_trait]
impl ModuleApi for Module {
    fn name(&self) -> &'static str {
        "MyMod"
    }

    fn description(&self) -> &'static str {
        "My first Rust module"
    }

    fn core(&self) -> bool {
        false
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![CommandDef::new("mycmd", "Does something").security(OWNER)]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        message.reply_html("Hello from MyMod!").await?;
        Ok(())
    }
}
```

## Installing

- From a URL: `.loadmod https://example.com/MyMod.rs`
- From a configured repository: `.dlmod MyMod.rs`
- On disk: drop the file here and run `.loadmod` for a URL, or rebuild manually
  with `cargo build --release`.

The loader copies the file into this directory, runs `cargo build --release`
and restarts the process. Build output is sent back to the chat; on failure the
process keeps running the previous binary.

## Buttons and callbacks

A module can render its own buttons and handle them:

```rust
use grammers_client::message::{Button, InputMessage, ReplyMarkup};

fn has_callback(&self) -> bool { true }

async fn on_callback(
    &self,
    _ctx: &Arc<Ctx>,
    call: &grammers_client::update::CallbackQuery,
    data: &str,
) -> Result<bool> {
    if data != "mymod:ping" {
        return Ok(false);
    }
    let _ = call.answer().text("pong").send().await;
    Ok(true)
}
```

Callbacks with the `heroku:` prefix are reserved for the core and handled
before module dispatch. By default only owners and sudo users may press your
buttons (same gate as Heroku's callback security); override `callback_allowed`
if a button is intentionally public.

## Watchers

The declarative API lets the dispatcher do the filtering, so your handler only
runs for matching updates:

```rust
use crate::module::{WatcherDef, WatcherFilter};

fn watchers(&self) -> Vec<WatcherDef> {
    vec![
        WatcherDef::new(
            "links",
            WatcherFilter {
                only_pm: true,
                no_commands: true,
                contains: Some("https://".to_owned()),
                ..Default::default()
            },
        ),
        WatcherDef::new(
            "edits",
            WatcherFilter {
                editable: Some(true),
                chat_id: Some(-1001234567890),
                ..Default::default()
            },
        ),
    ]
}

async fn on_watcher(
    &self,
    _ctx: &Arc<Ctx>,
    watcher: &str,
    message: &Msg,
    edited: bool,
) -> Result<bool> {
    // Return Ok(true) to stop command dispatch for this update.
    Ok(false)
}
```

`WatcherFilter` supports `no_commands`, `only_commands`, `outgoing`,
`editable`, `no_media`, `only_media`, `only_photos`, `only_videos`,
`only_audios`, `only_docs`, `only_stickers`, `no_photos`, `no_videos`,
`no_audios`, `no_docs`, `no_stickers`, `only_channels`, `only_groups`,
`only_pm`, `no_pm`, `no_channels`, `no_groups`, `no_forwards`,
`only_forwards`, `no_reply`, `only_reply`, `mention`, `no_mention`,
`startswith`, `endswith`, `contains`, `regex`, `from_id` and `chat_id`.
`only_docs` and `no_docs` both treat videos and audio files as documents,
because that is how Telegram stores them. The field name is `no_commands` (the
legacy `has_watcher()` + `on_message()` API is still supported, but it never
receives edits).

## Lifecycle, dependencies and errors

```rust
fn api_version(&self) -> u32 { crate::module::MODULE_API_VERSION }
fn dependencies(&self) -> &'static [&'static str] { &["Loader"] }

async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> { Ok(()) }
async fn on_unload(&self, ctx: &Arc<Ctx>) -> Result<()> { Ok(()) }
async fn on_error(&self, ctx: &Arc<Ctx>, source: &str, error: &anyhow::Error) {}
```

- Modules are loaded after their dependencies (names are case-insensitive and
  the `Mod` suffix is optional). Missing dependencies are ignored.
- `on_load` is skipped for modules disabled with `.togglemod`; toggling runs the
  hook immediately, without restarting the process.
- `on_error` is called for failed commands, watchers, callbacks and lifecycle
  hooks. Errors from it are logged and never propagate.

## Notes

- The same API as built-in modules is available: `ctx.db`, `ctx.inline`,
  configs, watchers, security flags and translations.
- `//!` inner doc comments are supported.
- Names must be unique; the loader strips a trailing `Mod` when resolving
  `.config <name>` and `.logs <name>`.
- `modules/` is git-ignored, except for this README and `example.rs`.
