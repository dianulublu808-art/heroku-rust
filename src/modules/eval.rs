// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.eval` — safe expression evaluation powered by Rhai.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, truncate, Msg};

pub struct Evaluator;

#[async_trait]
impl Module for Evaluator {
    fn name(&self) -> &'static str {
        "Evaluator"
    }

    fn description(&self) -> &'static str {
        "Evaluate expressions inside the userbot"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("e", "Evaluate a Rhai expression")
                .aliases(&["eval"])
                .usage("<expression>"),
            CommandDef::new("ec", "Evaluate a Rhai expression").usage("<expression>"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        _command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        let mut script = args.raw.trim().to_owned();
        if script.is_empty() {
            if let Some(reply) = message.get_reply().await? {
                script = reply.text().trim().to_owned();
            }
        }
        if script.is_empty() {
            message.reply_html(ctx.tr("eval.usage")).await?;
            return Ok(());
        }

        // Rhai values are intentionally `!Send` (they may hold `Rc`), so the
        // engine must live entirely inside a blocking worker. Only the rendered
        // strings cross the thread boundary.
        let outcome = tokio::task::spawn_blocking(move || {
            let mut engine = rhai::Engine::new();
            engine.set_max_operations(100_000);
            engine.set_max_string_size(100_000);
            engine.set_max_array_size(10_000);
            engine.set_max_map_size(10_000);

            match engine.eval_expression::<rhai::Dynamic>(&script) {
                Ok(value) => (true, value.to_string()),
                Err(err) => (false, err.to_string()),
            }
        })
        .await;

        match outcome {
            Ok((true, value)) => {
                message
                    .reply_html(ctx.tf("eval.result", &[&escape_html(&truncate(&value, 3800))]))
                    .await?;
            }
            Ok((false, error)) => {
                message
                    .reply_html(ctx.tf("eval.error", &[&escape_html(&truncate(&error, 3800))]))
                    .await?;
            }
            Err(err) => {
                message
                    .reply_html(ctx.tf("eval.error", &[&escape_html(&err.to_string())]))
                    .await?;
            }
        }
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(Evaluator))
}
