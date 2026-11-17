# 🪐 Heroku Userbot — Rust edition

A Telegram MTProto **userbot** rewritten from the Python project
[Heroku-userbot/Heroku](https://github.com/Heroku-userbot/Heroku) in Rust.

The goal of this repository is a fast, single-binary userbot that keeps the
familiar Heroku command surface, the same `heroku.json` database layout and the
same permission bitmasks, while replacing the Python runtime and virtualenv with
native Rust modules and keeping the module repository workflow.

> **Status:** working userbot with inline mode, multi-account switching, the
> module repository loader, backups and local API flood protection. Modules are
> real Rust files compiled into the binary — see
> [External Rust modules](#external-rust-modules).

---

## Features

- MTProto client built on [`grammers`](https://github.com/Lonami/grammers) 0.12
  (`Client`, `SenderPool`, SQLite session storage).
- First-run wizard, environment-variable overrides and an interactive login
  (including 2FA, delivery hints, `resend`, code retries and `TG_CODE` for
  automated starts).
- JSON database in the original `owner → key → value` shape, so an existing
  `heroku.json` can be reused.
- Command prefixes, personal prefixes, aliases, `| grep`, `@username` / `@me`
  targeting, nickname requirement, blacklist / whitelist and rate limiting.
- Permission system with the same bitmasks as the Python build: owners, sudo,
  support, security groups, temporary rules (`tsec`) and per-command masks.
- Built-in modules with configuration (`<module>.<key>` in the database) and
  language packs (`langpacks/en.yml`, `langpacks/ru.yml`).
- Real inline bot: card, help, command search, profile and terminal-in-inline,
  with callback buttons (restart, reset prefix, close) and bot PM commands.
- Multi-account support: `.addacc` performs a real MTProto login (code + 2FA)
  and `.switchacc` restarts the bot with another session/database.
- Module repository workflow: `.addrepo`, `.delrepo`, `.dlmall`, `.ml`,
  `.dlmod`, `.clearmodules` download real module files over HTTP(S), plus
  `.backupmods` / `.restoremods` archives.
- External Rust modules: any `modules/*.rs` file is compiled into the binary by
  `build.rs` and registered through `inventory`, so community modules have the
  same API and speed as built-in ones. The loader mirrors installed `.rs` files
  into the repository, rebuilds and restarts automatically.
- Local API flood protection with a sliding request window, freeze, owner
  warning and `.suspend_api_protect`.
- Graceful shutdown, colored logs kept in a ring buffer for `.logs`.

## Requirements

- Rust 1.85+ (edition 2021)
- A C toolchain and linker (`build-essential` on Debian, `clang`/`binutils` on Termux)
- Telegram `api_id` and `api_hash` from <https://my.telegram.org/apps>

SQLite is not required: sessions use the bundled pure-Rust `libsql` storage.

On Debian/Ubuntu:

```bash
sudo apt-get install build-essential
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```


## Build

The fastest way — the installer picks the right package manager, clones the
repository, builds the release binary and starts Heroku:

```bash
curl -fsSL https://raw.githubusercontent.com/dianulublu808-art/heroku-rust/main/install.sh | bash
```

Or build manually:

```bash
git clone https://github.com/dianulublu808-art/heroku-rust
cd heroku-rust
cargo build --release
./target/release/heroku
```

The binary is completely self-contained. Copy it to a server, put it in a
directory with your data, and run it.

### Termux (Android)

```bash
pkg install git rust binutils
git clone https://github.com/dianulublu808-art/heroku-rust
cd heroku-rust
cargo build --release
./target/release/heroku --data-root "$PWD/data"
```

Keep the session file private: anyone with `data/sessions/*.session` can use the
account.

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
| `TG_CODE` | One-shot login code (for non-interactive/automated starts) |
| `TG_2FA_PASSWORD` | Two-factor password (used once, then the prompt falls back to stdin) |
| `HEROKU_LANG` | Interface language (`en`, `ru`) |
| `LOG_LEVEL` | `trace`, `debug`, `info`, `warn`, `error` |
| `HEROKU_DATA_ROOT` | Data directory (same as `--data-root`) |

### First login

The wizard asks for `api_id`, `api_hash` and a phone number, then requests a
code. The bot prints **where** Telegram sent it:

- **Telegram app** — open the “Telegram” service chat (`777000`) on any device
  already signed in to this account. This is the usual channel for numbers that
  have an active Telegram session.
- **SMS** — can take a minute or two, and some carriers delay or drop Telegram
  messages.
- **Call / flash call / e-mail** — follow the printed instruction.

At the code prompt:

| Input | Effect |
| --- | --- |
| `12345` | Try to sign in |
| `resend` | Request a new code without restarting the bot |
| empty line | Ignore and ask again |

A wrong code can be retried up to three times; an expired code automatically
requests a new one. If 2FA is enabled, the password prompt shows the hint from
Telegram and allows three attempts.

The phone number may be entered as `+79001234567`, `0079001234567` or
`89001234567` (the last form is normalised to `+7…`). The wizard also asks for
the interface language; it can be changed later with `.setlang ru`.

If Telegram reports `FLOOD_WAIT`, wait the printed number of seconds before
requesting another code. Repeated restarts do not speed this up.

### Data directory

| Path | Contents |
| --- | --- |
| `config.json` | API credentials and preferences |
| `sessions/heroku.session` | SQLite MTProto session (keep private) |
| `heroku.json` | Userbot database |
| `accounts/<id>/` | Session and database of an additional account |
| `accounts.json` | Account registry and the active account |
| `modules/` | Modules downloaded from repositories |
| `backups/` | `.backupdb` / `.backupmods` output and restores |

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
| Core | `.help`, `.support`, `.heroku`, `.info`, `.settings`, `.ping`, `.id`, `.logs` |
| Config | `.config`, `.setprefix`, `.aliases`, `.addalias`, `.delalias` |
| Modules | `.togglemod`, `.togglecmd`, `.clearmodule`, `.blacklist`, `.unblacklist` |
| Security | `.owneradd`, `.ownerrm`, `.ownerlist`, `.newsgroup`, `.sgroupadd`, `.sgrouprule`, `.security`, `.tsec` |
| Backups | `.backupdb`, `.restoredb`, `.backupall`, `.restoreall`, `.backupmods`, `.restoremods`, `.set_backup_period` |
| Tools | `.terminal`, `.e` / `.eval`, `.ec`, `.ecpp`, `.ers`, `.eg`, `.enode`, `.tr`, `.setlang`, `.dllangpack` |
| Updates | `.update`, `.restart`, `.autoupdate`, `.source`, `.changelog` |
| Loader | `.ml`, `.dlmod`, `.loadmod`, `.unloadmod`, `.reloadmod`, `.loadall`, `.unloadall`, `.listmods`, `.modinfo`, `.addrepo`, `.delrepo`, `.dlmall`, `.clearmodules` |
| Inline | `.inline`, `.inlinestatus`, `.ch_bot_token`, `.ch_heroku_bot`, `.inlinesec`, `.querysec` |
| Accounts | `.addacc`, `.switchacc` |
| Protection | `.api_fw_protection`, `.suspend_api_protect` |

## Modules

| Module | Commands | Notes |
| --- | --- | --- |
| `Help` | `help`, `modhelp`, `support`, `helphide` | Module/command browser |
| `CoreMod` | `heroku`, `blacklist`, `setprefix`, `aliases`, `addalias`, `delalias`, `cleardb`, `togglecmd`, `togglemod`, `clearmodule`, `installation` | Core settings |
| `HerokuConfigMod` | `config` | Reads `config_spec()` of every module |
| `HerokuSecurityMod` | owners, security groups, `security`, `tsec*` | Same bitmasks as Python |
| `HerokuSettingsMod` | `settings`, `watchers`, `nonick*` | Global switches |
| `HerokuBackupMod` | `backupdb`, `restoredb`, `backupmods`, `restoremods`, `set_backup_period` | Database and module archives |
| `HerokuInfoMod` | `info`, `ubinfo` | Version, uptime, platform |
| `TestMod` | `ping`, `id`, `logs`, `clearlogs`, `suspend` | Self-testing |
| `Translations` | `setlang`, `dllangpack` | Language packs |
| `Translator` | `tr` | Google Translate backend |
| `TerminalMod` | `terminal`, `terminate` | Shell with dangerous-command guard, 120 s timeout |
| `Evaluator` | `e`, `eval`, `ec`, `ecpp`, `ers`, `eg`, `enode` | Rhai sandbox + real C/C++/Rust/Go/Node runners (Go only when installed) |
| `UpdaterMod` | `update`, `restart`, `autoupdate`, `source`, `changelog`, `rollback`, `ubstop` | Binary build: no self-update |
| `Quickstart` | — | First-run welcome message |
| `LoaderMod` | `ml`, `dlmod`, `loadmod`, `unloadmod`, `listmods`, `modinfo`, `addrepo`, `delrepo`, `dlmall`, `clearmodules` | Real HTTP(S) repository downloads into `modules/` |
| `LoaderRestrictor` | `allowmod`, `denymod`, `allowedmods` | Allow/deny list enforced by the loader |
| `InlineStuff` | `inline`, `inlinestatus`, `ch_bot_token`, `ch_heroku_bot`, `inlinesec`, `querysec` | Real inline bot with callback buttons |
| `HerokuAccounts` | `addacc`, `switchacc` | Multi-account login and restart-based switching |
| `APIRatelimiterMod` | `api_fw_protection`, `suspend_api_protect` | Sliding-window flood guard with owner warning |

## Architecture

```text
src/
  main.rs         entry point, banner, update loop, shutdown
  config.rs       config file, wizard, CLI, data layout
  login.rs        interactive login: delivery hints, resend, retries, 2FA
  accounts.rs     multi-account registry and active session/database paths
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

## External Rust modules

Modules are ordinary Rust files in `modules/`. A file exposes `pub struct
Module` implementing `crate::module::Module`; `build.rs` declares it as a crate
module and registers it with `inventory`. Install one from a URL:

```text
.loadmod https://example.com/MyMod.rs
```

The loader copies the file into the repository `modules/` directory, runs
`cargo build --release` and restarts. Build errors are reported in the chat and
the previous binary keeps running. See `modules/README.md` and
`modules/example.rs` for a complete template, and
[`docs/PORTING.md`](docs/PORTING.md) for a Python → Rust mapping of the module
API (commands, watchers, callbacks, config and lifecycle).

Python modules from the original Heroku repositories are still downloaded,
stored and backed up, but they are not executed: they have to be ported to
Rust.

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
