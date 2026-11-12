// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.terminal` — run shell commands.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use tokio::process::Command;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, truncate, Msg};

const DANGEROUS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "mkfs",
    "dd if=/dev/zero",
    ":(){",
    "shutdown",
    "reboot",
    "poweroff",
    "chmod -R 777 /",
    "> /dev/sda",
];

pub struct TerminalMod;

#[async_trait]
impl Module for TerminalMod {
    fn name(&self) -> &'static str {
        "TerminalMod"
    }

    fn description(&self) -> &'static str {
        "Run shell commands on the host"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("terminal", "Execute a shell command").usage("<command>"),
            CommandDef::new("terminate", "Stop a running command").usage("[-f]"),
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
            "terminal" => {
                let mut script = args.raw.trim().to_owned();
                if script.is_empty() {
                    if let Some(reply) = message.get_reply().await? {
                        script = reply.text().trim().to_owned();
                    }
                }
                if script.is_empty() {
                    message.reply_html(ctx.tr("terminal.usage")).await?;
                    return Ok(());
                }
                let lowered = script.to_lowercase();
                if DANGEROUS.iter().any(|needle| lowered.contains(needle)) {
                    message
                        .reply_html(format!(
                            "🚫 <b>Dangerous command blocked:</b> <code>{}</code>",
                            escape_html(script.lines().next().unwrap_or(""))
                        ))
                        .await?;
                    return Ok(());
                }

                let output = tokio::time::timeout(
                    Duration::from_secs(120),
                    Command::new("sh")
                        .arg("-lc")
                        .arg(&script)
                        .current_dir(&ctx.data_root)
                        .output(),
                )
                .await;

                let text = match output {
                    Ok(Ok(output)) => {
                        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                        text.push_str(&String::from_utf8_lossy(&output.stderr));
                        if text.trim().is_empty() {
                            format!("✅ exit code: {}", output.status.code().unwrap_or(-1))
                        } else {
                            text
                        }
                    }
                    Ok(Err(err)) => format!("🚫 {err}"),
                    Err(_) => "🚫 Command timed out after 120 seconds".to_owned(),
                };
                message
                    .reply_html(ctx.tf("terminal.result", &[&escape_html(&truncate(&text, 3800))]))
                    .await?;
            }
            "terminate" => {
                message
                    .reply_html("🚫 Process tracking is not available in the Rust build")
                    .await?;
            }
            _ => {}
        }
        Ok(())
    }
}

inventory::submit! {
    ModuleFactory(|| Box::new(TerminalMod))
}
