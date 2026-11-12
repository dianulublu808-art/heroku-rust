# 🪐 Heroku Userbot — Rust edition

A Telegram MTProto **userbot** rewritten from the Python project
[Heroku-userbot/Heroku](https://github.com/Heroku-userbot/Heroku) in Rust.

The goal of this repository is a fast, single-binary userbot that keeps the
familiar Heroku command surface, the same `heroku.json` database layout and the
same permission bitmasks, while removing the Python runtime, virtualenv and
runtime module loader.

> **Status:** working core. The bot logs in, dispatches commands, enforces
> permissions and ships the built-in modules listed below. Inline mode, the web
> panel, multi-account switching and dynamic Python module loading are **not**
> implemented yet (see [Roadmap](#roadmap)).

---

## Features

- MTProto client built on [`grammers`](https://github.com/Lonami/grammers) 0.12
  (`Client`, `SenderPool`, SQLite session storage).
- First-run wizard, environment-variable overrides and an interactive login
  (including 2FA).
- JSON database in the original `owner → key → value` shape, so an existing
  `heroku.json` can be reused.
- Command prefixes, personal prefixes, aliases, `| grep`, `@username` / `@me`
  targeting, nickname requirement, blacklist / whitelist and rate limiting.
- Permission system with the same bitmasks as the Python build: owners, sudo,
  support, security groups, temporary rules (`tsec`) and per-command masks.
- Built-in modules with configuration (`<module>.<key>` in the database) and
  language packs (`langpacks/en.yml`, `langpacks/ru.yml`).
- Graceful shutdown, colored logs kept in a ring buffer for `.logs`.

## Requirements

- Rust 1.85+ (edition 2021)
- A C toolchain, `pkg-config` and SQLite headers (`libsqlite3-dev` / `libsqlite`)
- Telegram `api_id` and `api_hash` from <https://my.telegram.org/apps>

On Debian/Ubuntu:

```bash
sudo apt-get install build-essential pkg-config libsqlite3-dev
```

## Build

```bash
cargo build --release
./target/release/heroku
```

The binary is completely self-contained. Copy it to a server, put it in a
directory with your data, and run it.

## Configuration

On first start the bot asks for `api_id`, `api_hash` and a phone number and
writes `config.json`:

```json
{
  "api_id": 123456,
  "api_hash": "0123456789abcdef0123456789abcdef",
  "phone": "+10000000000",
  "lang": "en",
  "log_level": "info",
  "session_name": "heroku"
}
```

Environment variables take priority over the file:

| Variable | Purpose |
| --- | --- |
| `TG_API_ID` | Telegram `api_id` |
| `TG_API_HASH` | Telegram `api_hash` |
| `TG_PHONE` | Phone number used for login |
| `HEROKU_LANG` | Interface language (`en`, `ru`) |
| `LOG_LEVEL` | `trace`, `debug`, `info`, `warn`, `error` |
| `HEROKU_DATA_ROOT` | Data directory (same as `--data-root`) |

### Data directory

| Path | Contents |
| --- | --- |
| `config.json` | API credentials and preferences |
| `sessions/heroku.session` | SQLite MTProto session (keep private) |
| `heroku.json` | Userbot database |
| `backups/` | `.backupdb` output and restores |

### CLI

```text
heroku [OPTIONS]

  --data-root <PATH>   Directory with config, sessions and database
  -w, --wipe           Delete config, sessions, database and backups
  --no-git             Do not use git for self-updates (accepted for parity)
  -v, --version        Print version
  -h, --help           Print help
```

## Docker

```bash
cp config.example.json config.json   # or run once interactively
docker compose up -d --build
docker compose exec worker heroku     # first interactive login
```

The compose file mounts a named volume at `/data` and uses `stdin_open: true`
so the login prompt works.

## Commands

Run `.help` for the live list. Highlights:

| Area | Commands |
| --- | --- |
| Core | `.help`, `.support`, `.heroku`, `.info`, `.settings`, `.ping`, `.logs` |
| Config | `.config`, `.setprefix`, `.aliases`, `.addalias`, `.delalias` |
| Modules | `.togglemod`, `.togglecmd`, `.clearmodule`, `.blacklist`, `.unblacklist` |
| Security | `.owneradd`, `.ownerrm`, `.ownerlist`, `.newsgroup`, `.sgroupadd`, `.sgrouprule`, `.security`, `.tsec` |
| Backups | `.backupdb`, `.restoredb`, `.backupall`, `.restoreall`, `.set_backup_period` |
| Tools | `.terminal`, `.e` / `.eval`, `.tr`, `.tr`, `.setlang` |
| Updates | `.update`, `.restart`, `.autoupdate`, `.source`, `.changelog` |
| Loader stubs | `.loadmod`, `.unloadmod`, `.listmods`, `.modinfo` (static build) |

## Modules

| Module | Commands | Notes |
| --- | --- | --- |
| `Help` | `help`, `modhelp`, `support`, `helphide` | Module/command browser |
| `CoreMod` | `heroku`, `blacklist`, `setprefix`, `aliases`, `addalias`, `delalias`, `cleardb`, `togglecmd`, `togglemod`, `clearmodule`, `installation` | Core settings |
| `HerokuConfigMod` | `config` | Reads `config_spec()` of every module |
| `HerokuSecurityMod` | owners, security groups, `security`, `tsec*` | Same bitmasks as Python |
| `HerokuSettingsMod` | `settings`, `watchers`, `nonick*` | Global switches |
| `HerokuBackupMod` | `backupdb`, `restoredb`, `set_backup_period` | Automatic backups |
| `HerokuInfoMod` | `info`, `ubinfo` | Version, uptime, platform |
| `TestMod` | `ping`, `logs`, `clearlogs`, `suspend` | Self-testing |
| `Translations` | `setlang`, `dllangpack` | Language packs |
| `Translator` | `tr` | Google Translate backend |
| `TerminalMod` | `terminal`, `terminate` | Shell with dangerous-command guard, 120 s timeout |
| `Evaluator` | `e`, `eval`, `ec` | Rhai sandbox (no host access) |
| `UpdaterMod` | `update`, `restart`, `autoupdate`, `source`, `changelog`, `rollback`, `ubstop` | Binary build: no self-update |
| `Quickstart` | — | First-run welcome message |
| `LoaderMod` | `listmods`, `modinfo`, load/unload stubs | Static build |
| `LoaderRestrictor` | `allowmod`, `denymod`, `allowedmods` | Static build |
| `InlineStuff` | `inline`, `inlinestatus` | Not implemented |
| `HerokuWebMod` | `web`, `webbind`, `websecret` | Not implemented |
| `APIRatelimiterMod` | `apilimiter` | Informational |

## Architecture

```text
src/
  main.rs         entry point, login, update loop, shutdown
  config.rs       config file, wizard, CLI, data layout
  database.rs     thread-safe JSON database
  dispatcher.rs   prefix parsing, permissions, rate limit, watchers
  module.rs       Module trait, CommandDef, inventory-based registry
  security.rs     owners, sudo, groups, masks, temporary rules
  tg.rs           grammers message wrapper (reply/edit/upload/download)
  translations.rs YAML language packs with fallbacks
  logbuf.rs       log ring buffer used by `.logs`
  modules/        every built-in module, one file per area
langpacks/
  en.yml, ru.yml
```

Modules register themselves at link time:

```rust
inventory::submit! { ModuleFactory(|| Box::new(MyModule)) }
```

Adding a file to `src/modules/mod.rs` is enough; there is no runtime plugin
loader.

### Security model

The command mask is `command.security` (or `0` = default), intersected with the
owner's **bounding mask**. A user also receives the following bits from their
role: owner, sudo, support, security group membership, chat admin rights and
being in PM. The bit values match Heroku/Hikka:

```text
owner=1 sudo=2 support=4 group_owner=8
group_admin_add_admins=16 ... group_admin=1024
group_member=2048 pm=4096 everyone=8192
```

## Roadmap

These parts of the Python project are intentionally **not** ported yet. The
corresponding commands answer with an honest "not available in the Rust build"
message instead of failing silently.

- [ ] Inline mode (`InlineStuff`, inline handlers, inline bot token)
- [ ] Web panel (`HerokuWebMod`, FastAPI UI, web auth)
- [ ] Multi-account switching and multiple sessions
- [ ] Dynamic Python module loading / `dlmod` / module repository
- [ ] Self-update from git (`updater` only reports status)
- [ ] `terminate` process tracking (commands have a timeout instead)
- [ ] Media-specific modules from the Python repo (notes, music, stickers, etc.)

## Development

```bash
cargo check
cargo test
cargo fmt
cargo clippy -- -D warnings
```

The database is plain JSON, so tests can create a temporary file and inspect it
directly.

## License

GNU Affero General Public License v3.0 — see [LICENSE](LICENSE). The original
Python project is licensed under the same terms.
