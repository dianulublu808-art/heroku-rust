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
before module dispatch.

## Watchers

Implement `has_watcher() -> true` and `on_message(...)` to see every incoming
message. Return `Ok(true)` to stop command dispatch for that message.

## Notes

- The same API as built-in modules is available: `ctx.db`, `ctx.inline`,
  configs, watchers, security flags and translations.
- `//!` inner doc comments are supported.
- Names must be unique; the loader strips a trailing `Mod` when resolving
  `.config <name>` and `.logs <name>`.
- `modules/` is git-ignored, except for this README and `example.rs`.
