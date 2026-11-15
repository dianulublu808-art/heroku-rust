// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Built-in modules. Each module registers itself through the `inventory`
//! crate, so adding a new file is enough to make it discoverable.

pub(crate) mod api_ratelimiter;
pub(crate) mod common;
mod eval;
mod help;
pub(crate) mod heroku_backup;
mod heroku_config;
mod heroku_info;
pub(crate) mod heroku_security;
pub(crate) mod heroku_settings;
mod inline;
pub mod loader;
pub(crate) mod loader_restrictor;
mod quickstart;
mod settings;
pub(crate) mod terminal;
pub(crate) mod test;
mod translate;
pub(crate) mod translations;
pub(crate) mod updater;
pub(crate) mod web;
