// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Example of an external Rust module.
//!
//! Any `modules/*.rs` file is compiled into the binary by `build.rs` and
//! registered automatically. A module file only has to expose
//! `pub struct Module` implementing `crate::module::Module`.
//!
//! Copy this file, rename the struct and its commands, and rebuild:
//!
//! ```text
//! .loadmod https://example.com/MyMod.rs
//! ```
//!
//! The loader keeps a copy in the repository `modules/` directory, runs
//! `cargo build --release` and restarts so the new commands appear.

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
        "ExampleMod"
    }

    fn description(&self) -> &'static str {
        "Example external Rust module"
    }

    fn author(&self) -> &'static str {
        "Heroku-userbot"
    }

    fn core(&self) -> bool {
        false
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![CommandDef::new("hello", "Say hello from an external Rust module")
            .aliases(&["hi"])
            .security(OWNER)]
    }

    async fn handle(
        &self,
        _ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        _args: &Args,
    ) -> Result<()> {
        message
            .reply_html("👋 <b>Hello from an external Rust module!</b>")
            .await?;
        Ok(())
    }
}
