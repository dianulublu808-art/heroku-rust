// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.terminal` — run shell commands with live output and `.terminate` to stop
//! them. Mirrors the Python terminal module (process groups, live redraw and
//! reply-based killing).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, truncate, Msg};

const MAX_OUTPUT: usize = 3500;
const RUN_TIMEOUT: Duration = Duration::from_secs(600);

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

struct Running {
    pid: u32,
}

/// Active commands keyed by the id of the message that started them.
static ACTIVE: LazyLock<Mutex<HashMap<i32, Running>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
            CommandDef::new("terminal", "Execute a shell command")
                .usage("<command>")
                .aliases(&["exec"]),
            CommandDef::new("terminate", "Stop a running command")
                .usage("[-f]")
                .aliases(&["kill"]),
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
            "terminal" => run(ctx, message, args).await,
            "terminate" => terminate(ctx, message, args).await,
            _ => Ok(()),
        }
    }
}

fn is_dangerous(command: &str) -> bool {
    let lowered = command.to_lowercase();
    DANGEROUS.iter().any(|needle| lowered.contains(needle))
}

async fn run(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
    let mut command = args.raw.trim().to_owned();
    if command.is_empty() {
        if let Some(reply) = message.get_reply().await? {
            command = reply.text().trim().to_owned();
        }
    }
    if command.is_empty() {
        message.reply_html(ctx.tr("terminal.usage")).await?;
        return Ok(());
    }
    if is_dangerous(&command) {
        message
            .reply_html(ctx.tf("terminal.dangerous_command", &[&escape_html(&command)]))
            .await?;
        return Ok(());
    }

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned());
    let mut builder = Command::new(shell);
    builder
        .arg("-lc")
        .arg(&command)
        .current_dir(&ctx.data_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("TERM", "dumb")
        .kill_on_drop(false);
    #[cfg(unix)]
    builder.process_group(0);

    let mut child = match builder.spawn() {
        Ok(child) => child,
        Err(err) => {
            message
                .reply_html(ctx.tf("terminal.exec_error", &[&escape_html(&format!("{err}"))]))
                .await?;
            return Ok(());
        }
    };

    let pid = child.id().unwrap_or_default();
    let response = message
        .reply_html(ctx.tf("terminal.running", &[&escape_html(&command)]))
        .await?;

    if let Ok(mut active) = ACTIVE.lock() {
        active.insert(message.id(), Running { pid });
    }

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let output = Arc::new(Mutex::new(String::new()));

    let reader_out = stdout.map(|stream| spawn_reader(stream, output.clone()));
    let reader_err = stderr.map(|stream| spawn_reader(stream, output.clone()));

    let started = Instant::now();
    let mut interval = tokio::time::interval(Duration::from_millis(1500));
    let status = loop {
        tokio::select! {
            result = child.wait() => break result.ok(),
            _ = interval.tick() => {
                let snapshot = snapshot(&output, &command, started, None);
                let _ = response.edit_html(&snapshot).await;
            }
        }
        if started.elapsed() > RUN_TIMEOUT {
            kill_group(pid, true);
            let _ = child.kill().await;
            break None;
        }
    };

    if let Some(task) = reader_out {
        let _ = task.await;
    }
    if let Some(task) = reader_err {
        let _ = task.await;
    }
    if let Ok(mut active) = ACTIVE.lock() {
        active.remove(&message.id());
    }

    let code = status.map(|status| status.code());
    let final_text = snapshot(&output, &command, started, code);
    let _ = response.edit_html(final_text).await;
    Ok(())
}

fn spawn_reader<R>(stream: R, output: Arc<Mutex<String>>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut buffer) = output.lock() {
                buffer.push_str(&line);
                buffer.push('\n');
                if buffer.len() > MAX_OUTPUT * 2 {
                    let excess = buffer.len() - MAX_OUTPUT;
                    let cut = buffer
                        .char_indices()
                        .nth(excess)
                        .map(|(index, _)| index)
                        .unwrap_or(buffer.len());
                    buffer.drain(..cut);
                }
            }
        }
    })
}

fn snapshot(
    output: &Arc<Mutex<String>>,
    command: &str,
    started: Instant,
    code: Option<Option<i32>>,
) -> String {
    let body = output
        .lock()
        .map(|buffer| buffer.clone())
        .unwrap_or_default();
    let body = if body.trim().is_empty() {
        "…".to_owned()
    } else {
        escape_html(&truncate(body.trim_end(), MAX_OUTPUT))
    };
    let elapsed = started.elapsed().as_secs_f64();
    match code {
        Some(Some(code)) => format!(
            "💻 <b>Terminal</b> <i>· exit {code} · {elapsed:.1}s</i>\n\
             <pre><code class=\"language-bash\">{body}</code></pre>"
        ),
        Some(None) => format!(
            "💻 <b>Terminal</b> <i>· killed · {elapsed:.1}s</i>\n\
             <pre><code class=\"language-bash\">{body}</code></pre>"
        ),
        None => format!(
            "💻 <b>Terminal</b> <i>· running · {elapsed:.1}s</i>\n\
             <pre><code class=\"language-bash\">{}</code></pre>\n\
             <pre><code class=\"language-bash\">{body}</code></pre>",
            escape_html(&truncate(command, 200))
        ),
    }
}

fn kill_group(pid: u32, forceful: bool) {
    let signal = if forceful { "KILL" } else { "TERM" };
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "kill -{signal} -{pid} 2>/dev/null || kill -{signal} {pid} 2>/dev/null"
        ))
        .status();
}

async fn terminate(ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
    let Some(reply) = message.get_reply().await? else {
        message.reply_html(ctx.tr("terminal.what_to_kill")).await?;
        return Ok(());
    };

    let target = ACTIVE
        .lock()
        .map(|active| active.get(&reply.id()).map(|running| running.pid))
        .unwrap_or(None);

    let Some(pid) = target else {
        message.reply_html(ctx.tr("terminal.no_cmd")).await?;
        return Ok(());
    };

    kill_group(pid, args.flag("f") || args.flag("force"));
    if let Ok(mut active) = ACTIVE.lock() {
        active.remove(&reply.id());
    }
    message.reply_html(ctx.tr("terminal.killed")).await?;
    Ok(())
}

inventory::submit! {
    ModuleFactory(|| Box::new(TerminalMod))
}
