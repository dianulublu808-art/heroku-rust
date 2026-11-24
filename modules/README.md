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
`startswith`, `endswith`, `contains`, `regex`, `from_id`, `chat_id`,
`only_inline` and `no_inline` (messages sent via a bot in inline mode).
`only_docs` and `no_docs` both treat videos and audio files as documents,
because that is how Telegram stores them. The field name is `no_commands` (the
legacy `has_watcher()` + `on_message()` API is still supported, but it never
receives edits).

Python's `filter=` callable maps to `with_func`. The predicate is stored in an
`Arc` and runs only after all declarative filters matched, so keep it cheap:

```rust
use crate::module::{WatcherDef, WatcherFilter};

fn watchers(&self) -> Vec<WatcherDef> {
    vec![WatcherDef::new(
        "short_text",
        WatcherFilter::default().with_func(|message: &Msg| message.text().len() < 280),
    )]
}
```

## Inline Manager 2.0: forms, lists and galleries

`ctx.inline` builds interactive units with TTL, pagination and owner checks.
Callback payloads are replaced with opaque per-unit tokens before they reach
Telegram; the original payload stays in memory.

```rust
use std::sync::Arc;

use crate::inline::{
    InlineButton, InlineCallbackAction, InlineMedia, InlinePage,
};

async fn show(ctx: &Arc<Ctx>, message: &Msg) -> Result<()> {
    let form = ctx
        .inline
        .form("<b>Choose an action</b>")
        .button(InlineButton::callback("Ping", "mymod:ping"))
        .button(InlineButton::url("Docs", "https://example.com"))
        .on_callback(
            "mymod:ping",
            Arc::new(|_ctx, event| {
                Box::pin(async move {
                    Ok(InlineCallbackAction::Toast(format!(
                        "pong from {}",
                        event.sender_id
                    )))
                })
            }),
        )
        .module("MyMod")
        .owner(message.sender_id().unwrap_or_default())
        .ttl(std::time::Duration::from_secs(600));

    let sent = form.send(ctx, message.chat_id()).await?;
    // sent.delete(ctx).await?;  // or sent.unload(ctx).await?;
    Ok(())
}
```

Lists paginate automatically:

```rust
let sent = ctx
    .inline
    .list((0..50).map(|i| format!("<b>{i}</b>. line")))
    .per_page(10)
    .header("📄 <b>Items</b>")
    .send(ctx, message.chat_id())
    .await?;
```

Galleries accept URL media and optional per-slide buttons:

```rust
let gallery = ctx.inline.gallery()
    .item(
        crate::inline::GalleryItem::new(
            InlineMedia::Photo("https://example.com/1.jpg".into()),
            "First slide",
        )
        .buttons(vec![vec![InlineButton::callback("Like", "mymod:like")]]),
    )
    .item(crate::inline::GalleryItem::new(
        InlineMedia::Photo("https://example.com/2.jpg".into()),
        "Second slide",
    ))
    .send(ctx, message.chat_id())
    .await?;
```

Media support:

- `InlineMedia::Photo` — external photo URL.
- `InlineMedia::Gif`/`Video`/`Audio`/`File` — external document URL. They are
  sent as documents, not as native animation/video/audio attributes.
- `InlineMedia::Location` — latitude/longitude map point.
- Inline **query** results remain article-based with title, description and
  thumbnail; native inline photo/document results are not implemented yet.

Security defaults to the bot owner + sudo. Use `.always_allow(vec![...])` for a
public unit, `.force_me(true)` to restrict it to the owner, or
`.disable_security(true)` only for intentionally public buttons. Units expire
after 24 hours by default; `.ttl(...)` can lower that, never raise it above 24
hours. The cleaner runs every five seconds, deletes the bot message and calls
the `.on_unload(...)` hook.

Callbacks are deduplicated by `(sender, query_id)` for 30 seconds, so Telegram
retries cannot run a handler twice. A unit checks TTL, owner and permissions
before invoking a handler.

### Module inline-query handlers

A module can answer `@bot <query>` itself:

```rust
use grammers_client::update::{Article, InlineQuery};
use crate::module::{InlineHandlerDef, InlineQueryReply};

fn inline_handlers(&self) -> Vec<InlineHandlerDef> {
    vec![InlineHandlerDef::new("mymod_gallery", Some("mygallery"))]
}

async fn on_inline_query(
    &self,
    ctx: &Arc<Ctx>,
    query: &InlineQuery,
    text: &str,
) -> Result<InlineQueryReply> {
    let gallery = ctx.inline.query_gallery()
        .item(crate::inline::QueryGalleryItem::new(
            "Example",
            "Opened from inline mode",
            crate::inline::InlinePage::new("Hello from MyMod"),
        ).thumb("https://example.com/thumb.jpg"))
        .answer(ctx, query)
        .await?;
    if gallery {
        Ok(InlineQueryReply::Answered)
    } else {
        Ok(InlineQueryReply::Skip)
    }
}
```

Handlers of disabled or failed modules are skipped. Return
`InlineQueryReply::Results(Vec<Article>)` to let the manager send the answer, or
`Answered` if you called `query.answer(...)` yourself.

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
- `MODULE_API_VERSION` is currently `3`. Version 3 added `WatcherFilter::func`;
  modules written for version 2 keep working because `WatcherFilter` is built
  with `..Default::default()`.

## Notes

- The same API as built-in modules is available: `ctx.db`, `ctx.inline`,
  configs, watchers, security flags and translations.
- `//!` inner doc comments are supported.
- Names must be unique; the loader strips a trailing `Mod` when resolving
  `.config <name>` and `.logs <name>`.
- `modules/` is git-ignored, except for this README and `example.rs`.
- Inline units store closures in memory, so they do not survive a process
  restart. This matches Python's `_units` lifetime.
