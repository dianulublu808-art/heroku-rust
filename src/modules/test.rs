// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Ping, logs and self-testing helpers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, Msg};

pub struct TestMod;

#[async_trait]
impl Module for TestMod {
    fn name(&self) -> &'static str {
        "TestMod"
    }

    fn description(&self) -> &'static str {
        "Perform operations based on userbot self-testing"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("ping", "Measure the userbot latency"),
            CommandDef::new("logs", "Show recent logs").usage("[count]"),
            CommandDef::new("clearlogs", "Clear the log buffer"),
            CommandDef::new("suspend", "Pause command handling").usage("<seconds>"),
        ]
    }

    async fn handle(
        &self,
        ctx: &Arc<Ctx>,
        command: &str,
        message: &Msg,
        args: &Args,
    ) -> Result<()> {
        match command {
            "ping" => {
                let started = Instant::now();
                let response = message.respond_html("🪐").await?;
                let latency = started.elapsed().as_millis();
                response
                    .edit_html(ctx.tf("test.pong", &[&latency.to_string()]))
                    .await?;
            }
            "logs" => {
                let count = args
                    .get(0)
                    .and_then(|raw| raw.parse::<usize>().ok())
                    .unwrap_or(50)
                    .clamp(1, 200);
                let lines = ctx.log_buffer.tail(count);
                let body = escape_html(&lines.join("\n"));
                message
                    .reply_html(format!(
                        "🧾 <b>Logs</b> <i>({}/{})</i>\n<pre><code class=\"language-logs\">{body}</code></pre>",
                        lines.len(),
                        count
                    ))
                    .await?;
            }
            "clearlogs" => {
                ctx.log_buffer.clear();
                message.reply_html(ctx.tr("test.logs_cleared")).await?;
            }
            "suspend" => {
                let seconds = args
                    .get(0)
                    .and_then(|raw| raw.parse::<f64>().ok())
                    .unwrap_or(0.0)
                    .clamp(0.0, 3600.0);
                message
                    .reply_html(format!("⏸ <b>Suspended for {seconds:.1}s</b>"))
                    .await?;
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                message.reply_html(ctx.tr("test.resumed")).await?;
            }
            _ => {}
        }
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(TestMod))
}
