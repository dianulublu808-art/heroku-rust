// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.terminal` — run shell commands with live output and `.terminate` to stop
//! them. Mirrors the Python terminal module (process groups, live redraw and
//! reply-based killing).

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, Command};
use tokio::sync::Mutex as AsyncMutex;

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

#[derive(Clone)]
struct SudoSession {
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    prompted: Arc<AtomicBool>,
}

/// Active commands keyed by the id of the message that started them.
static ACTIVE: LazyLock<Mutex<HashMap<i32, Running>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Processes waiting for a sudo password, keyed by `(chat_id, message_id)`.
static SUDO: LazyLock<Mutex<HashMap<(i64, i32), SudoSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// Fallback when the inline bot is unavailable: chat id -> command message id.
static PENDING_CHAT_SUDO: LazyLock<Mutex<HashMap<i64, i32>>> =
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

    fn has_watcher(&self) -> bool {
        true
    }

    async fn on_message(&self, ctx: &Arc<Ctx>, message: &Msg) -> Result<bool> {
        let chat_id = message.chat_id();
        let pending = PENDING_CHAT_SUDO
            .lock()
            .ok()
            .and_then(|pending| pending.get(&chat_id).copied());
        let Some(message_id) = pending else {
            return Ok(false);
        };
        if message.sender_id() != Some(ctx.me_id) {
            return Ok(false);
        }
        let text = message.text().trim().to_owned();
        if text.is_empty() {
            return Ok(false);
        }
        if let Ok(mut pending) = PENDING_CHAT_SUDO.lock() {
            pending.remove(&chat_id);
        }
        let _ = message.delete().await;
        let html = submit_sudo_password(ctx, chat_id, message_id, &text).await;
        message.reply_html(html).await?;
        Ok(true)
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
    let fish = std::path::Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "fish");
    let script = if fish {
        format!(
            "function sudo\n    command sudo -S -p '[heroku-sudo] password:' $argv\nend\n{command}"
        )
    } else {
        format!("sudo() {{ command sudo -S -p '[heroku-sudo] password:' \"$@\"; }};\n{command}")
    };
    let mut builder = Command::new(&shell);
    builder
        .arg("-lc")
        .arg(&script)
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

    let sudo_key = (message.chat_id(), message.id());
    let stdin_handle = Arc::new(AsyncMutex::new(child.stdin.take()));
    let prompted = Arc::new(AtomicBool::new(false));

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
                if !prompted.load(Ordering::Relaxed) && sudo_prompt_seen(&output) {
                    prompted.store(true, Ordering::Relaxed);
                    if let Ok(mut sudo) = SUDO.lock() {
                        sudo.insert(
                            sudo_key,
                            SudoSession {
                                stdin: stdin_handle.clone(),
                                prompted: prompted.clone(),
                            },
                        );
                    }
                    let prompt = ctx.tr("terminal.sudo_prompt");
                    if ctx
                        .inline
                        .send_to_user(ctx, ctx.me_id, prompt, Vec::new())
                        .await
                        .is_ok()
                    {
                        ctx.inline.set_pending(
                            ctx.me_id,
                            crate::inline::PendingInput::SudoPassword {
                                chat_id: sudo_key.0,
                                message_id: sudo_key.1,
                            },
                        );
                    } else {
                        if let Ok(mut pending) = PENDING_CHAT_SUDO.lock() {
                            pending.insert(sudo_key.0, sudo_key.1);
                        }
                        let _ = message.reply_html(ctx.tr("terminal.sudo_chat_prompt")).await;
                    }
                }
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
    if let Ok(mut sudo) = SUDO.lock() {
        sudo.remove(&sudo_key);
    }
    if let Ok(mut pending) = PENDING_CHAT_SUDO.lock() {
        if pending.get(&sudo_key.0) == Some(&sudo_key.1) {
            pending.remove(&sudo_key.0);
        }
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

fn sudo_prompt_seen(output: &Arc<Mutex<String>>) -> bool {
    output
        .lock()
        .map(|buffer| buffer.contains("[heroku-sudo] password:"))
        .unwrap_or(false)
}

/// Feed a sudo password to a running terminal command and report the result.
pub(crate) async fn submit_sudo_password(
    ctx: &Arc<Ctx>,
    chat_id: i64,
    message_id: i32,
    password: &str,
) -> String {
    let session = SUDO
        .lock()
        .ok()
        .and_then(|sudo| sudo.get(&(chat_id, message_id)).cloned());
    let Some(session) = session else {
        return ctx.tr("terminal.sudo_expired");
    };

    let mut guard = session.stdin.lock().await;
    let Some(stdin) = guard.as_mut() else {
        return ctx.tr("terminal.sudo_expired");
    };
    match stdin.write_all(format!("{password}\n").as_bytes()).await {
        Ok(()) => {
            let _ = stdin.flush().await;
            session.prompted.store(false, Ordering::Relaxed);
            ctx.tr("terminal.sudo_sent")
        }
        Err(err) => ctx.tf("terminal.sudo_failed", &[&escape_html(&err.to_string())]),
    }
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
