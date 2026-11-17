# Heroku Changelog

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
