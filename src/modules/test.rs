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
            CommandDef::new("id", "Show user, chat and message identifiers"),
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
            "id" => {
                let sender = message
                    .sender_id()
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                let kind = if message.is_private() {
                    ctx.tr("test.type_private")
                } else if message.is_channel() {
                    ctx.tr("test.type_channel")
                } else {
                    ctx.tr("test.type_group")
                };
                message
                    .reply_html(format!(
                        "{title}\n{user}\n{chat}\n{message_id}\n{kind}",
                        title = ctx.tr("test.ids_title"),
                        user = ctx.tf("test.id_user", &[&sender]),
                        chat = ctx.tf("test.id_chat", &[&message.chat_id().to_string()]),
                        message_id = ctx.tf("test.id_message", &[&message.id().to_string()]),
                        kind = ctx.tf("test.id_type", &[&kind]),
                    ))
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
                        "{title}\n<pre><code class=\"language-logs\">{body}</code></pre>",
                        title = ctx.tf(
                            "test.logs_title",
                            &[&lines.len().to_string(), &count.to_string()]
                        )
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
                    .reply_html(ctx.tf("test.suspended_for", &[&format!("{seconds:.1}")]))
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
