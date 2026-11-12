// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Built-in modules. Each module registers itself through the `inventory`
//! crate, so adding a new file is enough to make it discoverable.

mod api_ratelimiter;
mod common;
mod eval;
mod help;
mod heroku_backup;
mod heroku_config;
mod heroku_info;
mod heroku_security;
mod heroku_settings;
mod inline;
mod loader;
mod quickstart;
mod settings;
mod terminal;
mod test;
mod translate;
mod translations;
mod updater;
mod web;
