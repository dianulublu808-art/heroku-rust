# Porting a Python Heroku module to Rust

Heroku Rust does **not** execute Python. Every Python module must be ported to a
Rust source file that is compiled into the binary. This document maps the Python
module API used by [`ZetGoHack/Heroku`](https://github.com/ZetGoHack/Heroku) to
the Rust API in [`src/module.rs`](../src/module.rs).

The short version:

| Python | Rust |
| --- | --- |
| module class with `strings = {"name": ...}` | `impl Module`, `fn name()` |
| `@loader.command` | `commands()` returning `CommandDef` |
| `@loader.watcher` + `@loader.tag` | `watchers()` returning `WatcherDef` |
| `@loader.callback_handler` | `has_callback()` + `on_callback()` |
| `@loader.inline_handler` | not exposed to external modules yet |
| `@loader.raw_handler` | not exposed to external modules yet |
| `@loader.ratelimit` | `CommandDef::ratelimit()` |
| `@loader.owner` / `@loader.unrestricted` | `CommandDef::security(OWNER)` / `EVERYONE` |
| `self.config` + `config_complete()` | `config_spec()` + `ctx.db` |
| `async def client_ready()` | `async fn on_load()` |
| `async def on_unload()` | `async fn on_unload()` |
| `@loader.translatable_docstring` | doc comments + langpacks, see below |
| `self.libraries` | split into modules or use crates |
| aliases (`@loader.tag(alias=...)`, `self.aliases`) | `CommandDef::aliases()` |

## 1. The smallest possible module

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
        "HelloMod" // must be unique; `.help` and `.togglemod` use it
    }

    fn description(&self) -> &'static str {
        "Says hello"
    }

    fn core(&self) -> bool {
        false // external modules are not core and can be disabled
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![CommandDef::new("hello", "Say hello")
            .aliases(&["hi", "hey"])
            .usage("[name]")
            .security(OWNER)]
    }

    async fn handle(
        &self,
        _ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        let name = args.get(0).unwrap_or("world");
        message
            .reply_html(format!("Hello, {}!", crate::tg::escape_html(name)))
            .await?;
        Ok(())
    }
}
```

Drop the file into `modules/` and rebuild, or use `.loadmod https://…/HelloMod.rs`.
The file name must match the module name closely enough for `.logs`; the compiled
tracing target is `heroku::ext_<stem>`.

## 2. Commands and aliases

Python:

```python
@loader.command
async def hello(self, message):
    ...
```

Rust declares all commands up front. `handle()` receives the command name that
was actually invoked, so aliases can branch if needed:

```rust
fn commands(&self) -> Vec<CommandDef> {
    vec![
        CommandDef::new("hello", "Say hello").aliases(&["hi"]),
        CommandDef::new("hello_reset", "Reset the greeting").security(OWNER),
    ]
}

async fn handle(&self, ctx: &Arc<Ctx>, command: &str, message: &Msg, args: &Args) -> Result<()> {
    match command {
        "hello" | "hi" => message.reply_html("Hello!").await?,
        "hello_reset" => message.reply_html("Reset.").await?,
        _ => {}
    }
    Ok(())
}
```

`Args` is already parsed: `args.get(n)`, `args.join(n..)`, `args.flag("c")`,
`args.value("m")`, `args.is_empty()`. Quoted strings are supported.

### Message helpers

| Python (Telethon) | Rust |
| --- | --- |
| `await message.answer(text)` | `message.reply_html(text).await?` |
| `await message.respond(text)` | `message.respond_html(text).await?` |
| `await message.edit(text)` | `message.edit_html(text).await?` |
| `await message.delete()` | `message.delete().await?` |
| `message.raw_text` | `message.text()` |
| `message.sender_id` | `message.sender_id()` |
| `message.chat_id` | `message.chat_id()` |
| `message.out` | `message.outgoing()` |
| `message.is_private` | `message.is_private()` |
| `message.reply_to_msg_id` | `message.reply_to_id()` |

Always escape user input with `crate::tg::escape_html` before sending HTML.

### Rate limits and permissions

- `.ratelimit()` on a `CommandDef` enables per-user command rate limiting.
- `CommandDef::security(mask)` takes the same bitmasks as Python Heroku:
  `OWNER`, `SUDO`, `SUPPORT`, `GROUP_OWNER`, `GROUP_ADMIN*`, `GROUP_MEMBER`,
  `PM`, `EVERYONE`. Masks are still intersected with the database bounding mask.
- Python's `@loader.unrestricted` is `security(EVERYONE)`.
- Python's `@loader.pm` is `security(PM)`. Use `PM | OWNER` if the owner should
  always be allowed.

## 3. Watchers

Python describes filters declaratively with `@loader.tag`:

```python
@loader.watcher("no_commands", "only_pm")
async def watcher(self, message):
    ...
```

Rust uses `WatcherFilter` and a named `WatcherDef`:

```rust
use crate::module::{WatcherDef, WatcherFilter};

fn watchers(&self) -> Vec<WatcherDef> {
    vec![WatcherDef::new(
        "pm_links",
        WatcherFilter {
            no_commands: true,
            only_pm: true,
            contains: Some("https://".to_owned()),
            ..Default::default()
        },
    )]
}

async fn on_watcher(
    &self,
    ctx: &Arc<Ctx>,
    watcher: &str,
    message: &Msg,
    edited: bool,
) -> Result<bool> {
    if watcher == "pm_links" {
        // ... handle the link ...
        return Ok(true); // stop command dispatch for this message
    }
    Ok(false)
}
```

Supported filters:

`no_commands`, `only_commands`, `outgoing: Some(bool)`, `editable: Some(bool)`,
`no_media`, `only_media`, `only_photos`, `only_videos`, `only_audios`,
`only_docs`, `only_stickers`, `no_photos`, `no_videos`, `no_audios`,
`no_docs`, `no_stickers`, `only_channels`, `only_groups`, `only_pm`,
`no_pm`, `no_channels`, `no_groups`, `no_forwards`, `only_forwards`,
`no_reply`, `only_reply`, `mention`, `no_mention`, `startswith`, `endswith`,
`contains`, `regex`, `from_id`, `chat_id`.

Notes:

- Only `editable: Some(true)` receives message edits. This mirrors Python's
  default `editable=False`.
- `only_docs` matches documents, videos and audio files (Telegram stores them as
  documents).
- `only_inline` / `no_inline` have no equivalent yet: message watchers never
  receive inline queries.
- The legacy `has_watcher() -> true` + `on_message()` pair still works for one
  unfiltered watcher, but it never sees edits. Prefer `watchers()`.
- `has_any_watcher()` does not need to be implemented: it is derived from
  `has_watcher()` and `watchers()`.

## 4. Callbacks

Python:

```python
@loader.callback_handler
async def my_callback(self, call, data):
    ...
```

Rust external modules receive callbacks through `on_callback`:

```rust
use grammers_client::update::CallbackQuery;

fn has_callback(&self) -> bool {
    true
}

async fn on_callback(
    &self,
    _ctx: &Arc<Ctx>,
    call: &CallbackQuery,
    data: &str,
) -> Result<bool> {
    let Some(rest) = data.strip_prefix("hello:") else {
        return Ok(false); // not ours
    };
    // ...
    let _ = call.answer().send().await;
    Ok(true) // consumed
}
```

`heroku:*` payloads are reserved by the core and are handled before module
callbacks. By default only owners and sudo users may press module buttons. If a
button is intentionally public, override:

```rust
fn callback_allowed(&self, _ctx: &Arc<Ctx>, _sender: i64) -> bool {
    true
}
```

Inline handlers (`@loader.inline_handler`) and raw TL handlers
(`@loader.raw_handler`) are not part of the external-module API yet. Use a core
module or extend the host if you genuinely need them.

## 5. Configuration

Python modules declare `self.config = loader.ModuleConfig(...)`, use
`self.config["key"]` and react in `config_complete()`.

Rust declares `ConfigSpec` entries. `.config`, the inline settings card and
`.togglemod` use this metadata, while values live in the shared database under
the module owner (the module name):

```rust
use crate::module::{ConfigDefault, ConfigKind, ConfigSpec};

fn config_spec(&self) -> Vec<ConfigSpec> {
    vec![
        ConfigSpec::new(
            "enabled",
            "Enable the watcher",
            ConfigDefault::Bool(true),
            ConfigKind::Bool,
        ),
        ConfigSpec::new(
            "mode",
            "Processing mode",
            ConfigDefault::Text("safe"),
            ConfigKind::Choice(&["safe", "fast"]),
        ),
    ]
}

async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> {
    // Declared defaults are available through the same getters as stored values.
    let enabled = ctx.db.get_bool(self.name(), "enabled", true);
    let mode = ctx.db.get_str(self.name(), "mode", "safe");
    tracing::debug!("{} loaded: enabled={enabled} mode={mode}", self.name());
    Ok(())
}
```

Database access from a module should always use `self.name()` as the owner (or
an explicit sub-namespace such as `"heroku.main"` for global settings). Useful
methods: `get_str`, `get_bool`, `get_i64`, `get_string_list`, `get_i64_list`,
`get_map`, `get_array`, `set`, `remove`.

`config_complete()` has no separate hook: `on_load()` runs after configuration
defaults exist, which is the closest equivalent. Do not rewrite values in
`on_load`; the host already applies defaults when a config is first created.

### Translations

Python's `@loader.translatable_docstring` and `self.strings` are replaced by
langpacks. Keep user-visible text in `langpacks/*.yml`, or at minimum pass it
through `ctx.tr()` when adding strings to the core langpacks. External modules
that ship their own strings should document that they are not translated.

## 6. Lifecycle, dependencies and errors

```rust
fn api_version(&self) -> u32 { crate::module::MODULE_API_VERSION }

fn dependencies(&self) -> &'static [&'static str] {
    &["Loader"] // loaded first; missing dependencies are ignored
}

async fn on_load(&self, ctx: &Arc<Ctx>) -> Result<()> { Ok(()) }
async fn on_unload(&self, ctx: &Arc<Ctx>) -> Result<()> { Ok(()) }

async fn on_error(&self, ctx: &Arc<Ctx>, source: &str, error: &anyhow::Error) {
    tracing::warn!("{} failed in {source}: {error:#}", self.name());
}
```

- `on_load` is skipped while a module is disabled with `.togglemod`. Enabling it
  later calls `on_load` immediately; disabling calls `on_unload`, without a
  process restart.
- `dependencies()` is used for topological `on_load` order. Names are
  case-insensitive and the `Mod` suffix is optional. Cycles fall back to name
  order and log a warning instead of dropping modules.
- `on_error` is called for failed commands, watchers, module callbacks and
  lifecycle hooks. It must not panic; its own errors are logged.
- `api_version()` is informational today: the host warns when a module targets
  a newer API than the running build. Keep the default unless you know why.

## 7. Libraries and imports

Python's `self.libraries` (load another `.py` file) has no direct equivalent.
Options, in order of preference:

1. Put the shared code in its own Rust module and declare it as a dependency.
2. Use a normal crate dependency.
3. Merge the code into the module file if it is small.

Dynamic import of another external `.rs` file at runtime is not supported.

## 8. Porting checklist

- [ ] Module name is unique and stable; it becomes the DB owner.
- [ ] Every Python command has a `CommandDef` with the same name and aliases.
- [ ] Command security matches the Python decorators, not a broader mask.
- [ ] Watchers use `WatcherFilter`; edits are requested explicitly.
- [ ] Callback payloads are namespaced and `has_callback()` is set.
- [ ] Config keys keep their old names so existing databases keep working.
- [ ] `client_ready` logic moved to `on_load`; cleanup moved to `on_unload`.
- [ ] Errors use `anyhow::Result`; no `unwrap`/`expect` on user input or network.
- [ ] User input is escaped before HTML output.
- [ ] `.loadmod`/rebuild works, `.help` lists the commands, `.logs <Module>`
      shows the module's tracing target.

## 9. Deliberate incompatibilities

- Python source is never executed; there is no interpreter fallback.
- Module code is compiled into the binary. "Hot reload" means recompiling and
  restarting the process.
- `quote_media` is a link with a preview, not a native quote: grammers 0.12 has
  no `InputMediaWebPage`.
- `.eval` runs Rhai, not Python, so only Rust/Rhai snippets work.
- Inline handlers and raw TL handlers are not exposed to external modules yet.
- `.terminal` is a trusted owner tool, not a sandbox.
