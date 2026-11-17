# Heroku Changelog

## Unreleased

 - security: module callbacks are owner/sudo only by default, `.logs`/`profile`
   cards verify the sender, `.help` is filtered by the security mask
 - database: atomic unique-temp writes with file and directory `fsync`, a save
   mutex, corrupt-file preservation and a pre-migration snapshot
 - database: schema version in `heroku.meta.schema`; a database written by a
   newer build is opened read-only and never rewritten or downgraded; namespace
   migrations are persisted immediately
 - accounts: corrupted `accounts.json` is preserved and a missing active
   session falls back with a warning
 - dispatcher: bounded update concurrency (64), graceful drain on shutdown and
   lifecycle hooks only for enabled modules
 - module API 2: `WatcherFilter` (including `no_*` media filters with `no_docs`
   symmetric to `only_docs`), named watchers with edits, `on_error`,
   `dependencies()`, `MODULE_API_VERSION`
 - watcher regexes are cached (bounded to 256 patterns, invalid patterns too)
 - tests: dependency cycles/self-dependencies, dispatcher blocklist policies,
   inline callback/pending state, schema downgrade protection and corrupt
   database recovery
 - `.togglemod` runs `on_load`/`on_unload` without restarting the process
 - inline: duplicate callback queries are suppressed for 30 seconds
 - logs: tokens, passwords, `api_hash`, Bearer headers and URL credentials are
   redacted before entering the ring buffer
 - docs: `ROADMAP.md` with the parity map and `docs/PORTING.md` with the
   Python → Rust module porting guide
 - install: Arch/pacman support, `curl` in Termux and a post-rustup cargo check

## 🪐 Heroku Rust 1.2.0

 - full rewrite of the Heroku userbot in Rust on top of `grammers`
 - every Python command, alias and inline action ported without stubs
 - multi-account registry: `.addacc`, `.switchacc` with an inline confirmation card
 - Python-compatible backups: `.backupdb`, `.backupmods`, `.backupall` and their restore counterparts
 - automatic backups with an inline period picker and channel upload
 - real inline bot with help, config, settings, security, accounts, modules, backups, API, language and logs pages
 - interactive installation picker for VDS, WSL, UserLAnd, HikkaHost and universal setups
 - module loader with repositories, HTTP downloads, `.dlmod`, `.loadmod`, `.reloadmod` and an inline gallery
 - external Rust modules: `modules/*.rs` are compiled by `build.rs`, registered through `inventory`, rebuilt and reloaded by the loader
 - `install.sh` universal installer for Termux, Debian/Ubuntu/WSL/UserLAnd and Alpine
 - loader safety quiz (`LoaderRestrictor`) with poll verification and per-account gating
 - API flood protection: sliding window, freeze, suspend, warnings and an inline configuration card
 - terminal with sudo password flow, `.terminate` by reply and `-f`
 - Rhai evaluator plus real `.ec`, `.ecpp`, `.ers`, `.eg` and `.enode` commands
 - nine language packs with a completeness test and community download support
 - autoupdate poller with major/minor handling, `.changelog` and `.rollback`
 - graceful shutdown, log buffer, `.logs` level and module filters
 - QR-code and code-based login with 2FA support
