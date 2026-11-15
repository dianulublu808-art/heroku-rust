// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! `.eval` — evaluate Rhai expressions, plus the language playground commands
//! (`.ec`, `.ecpp`, `.ers`, `.eg`, `.enode`) that really compile and run code on
//! the host.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use rand::Rng;
use tokio::process::Command;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, ModuleFactory};
use crate::tg::{escape_html, truncate, Msg};

const COMPILE_TIMEOUT: Duration = Duration::from_secs(30);
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Evaluator;

#[async_trait]
impl Module for Evaluator {
    fn name(&self) -> &'static str {
        "Evaluator"
    }

    fn description(&self) -> &'static str {
        "Evaluate expressions and run code in C, C++, Rust, Go and Node"
    }

    fn commands(&self) -> Vec<CommandDef> {
        vec![
            CommandDef::new("e", "Evaluate a Rhai expression")
                .aliases(&["eval"])
                .aliases(&["eval"])
                .usage("<expression>"),
            CommandDef::new("ec", "Compile and run C code").usage("<code>"),
            CommandDef::new("ecpp", "Compile and run C++ code").usage("<code>"),
            CommandDef::new("ers", "Compile and run Rust code").usage("<code>"),
            CommandDef::new("eg", "Run Go code").usage("<code>"),
            CommandDef::new("enode", "Run Node.js code").usage("<code>"),
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
            "ec" => self.compile(ctx, message, args, Language::C).await,
            "ecpp" => self.compile(ctx, message, args, Language::Cpp).await,
            "ers" => self.compile(ctx, message, args, Language::Rust).await,
            "eg" => self.compile(ctx, message, args, Language::Go).await,
            "enode" => self.compile(ctx, message, args, Language::Node).await,
            _ => self.rhai(ctx, message, args).await,
        }
    }
}

impl Evaluator {
    async fn rhai(&self, ctx: &Arc<Ctx>, message: &Msg, args: &Args) -> Result<()> {
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

    async fn compile(
        &self,
        ctx: &Arc<Ctx>,
        message: &Msg,
        args: &Args,
        language: Language,
    ) -> Result<()> {
        let mut code = args.raw.trim().to_owned();
        if code.is_empty() {
            if let Some(reply) = message.get_reply().await? {
                code = reply.text().trim().to_owned();
            }
        }
        if code.is_empty() {
            message
                .reply_html(ctx.tf("eval.code_usage", &[language.entry()]))
                .await?;
            return Ok(());
        }

        if !language.available().await {
            message
                .reply_html(ctx.tf(
                    "eval.no_compiler",
                    &[&language.display(), &language.command()],
                ))
                .await?;
            return Ok(());
        }

        let status = message.reply_html(ctx.tr("eval.compiling")).await?;

        let (ok, output) = language.run(code.clone()).await;
        let template = if ok {
            "eval.code_result"
        } else {
            "eval.code_error"
        };
        let rendered = ctx.tn(
            template,
            &[
                ("lang", &language.code_lang()),
                ("name", &language.display()),
                ("code", &escape_html(&truncate(&code, 3000))),
                ("result", &escape_html(&truncate(&output, 3200))),
            ],
        );

        if status.edit_html(rendered.clone()).await.is_err() {
            message.reply_html(rendered).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Language {
    C,
    Cpp,
    Rust,
    Go,
    Node,
}

impl Language {
    fn display(self) -> String {
        match self {
            Self::C => "C (gcc)".to_owned(),
            Self::Cpp => "C++ (g++)".to_owned(),
            Self::Rust => "Rust (rustc)".to_owned(),
            Self::Go => "Go (go run)".to_owned(),
            Self::Node => "Node.js".to_owned(),
        }
    }

    fn command(self) -> String {
        match self {
            Self::C => "gcc".to_owned(),
            Self::Cpp => "g++".to_owned(),
            Self::Rust => "rustc".to_owned(),
            Self::Go => "go".to_owned(),
            Self::Node => "node".to_owned(),
        }
    }

    fn code_lang(self) -> String {
        match self {
            Self::C => "c".to_owned(),
            Self::Cpp => "cpp".to_owned(),
            Self::Rust => "rust".to_owned(),
            Self::Go => "go".to_owned(),
            Self::Node => "javascript".to_owned(),
        }
    }

    fn entry(self) -> &'static str {
        match self {
            Self::C => "ec",
            Self::Cpp => "ecpp",
            Self::Rust => "ers",
            Self::Go => "eg",
            Self::Node => "enode",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::C => "code.c",
            Self::Cpp => "code.cpp",
            Self::Rust => "code.rs",
            Self::Go => "code.go",
            Self::Node => "code.js",
        }
    }

    async fn available(self) -> bool {
        let arg = match self {
            Self::Go => "version",
            _ => "--version",
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(self.command()).arg(arg).output(),
        )
        .await
        .map(|result| {
            result
                .map(|output| output.status.success())
                .unwrap_or(false)
        })
        .unwrap_or(false)
    }

    async fn run(self, code: String) -> (bool, String) {
        // Go compiles in a module-aware directory, so keep the temp folder for
        // the whole run and only clean it up at the end.
        let dir = temp_dir();
        if fs::create_dir_all(&dir).is_err() {
            return (false, "Failed to create a temporary directory".to_owned());
        }

        let source = dir.join(self.file_name());
        if let Err(err) = fs::write(&source, code) {
            cleanup(&dir);
            return (false, format!("Failed to write source file: {err}"));
        }

        let result = match self {
            Self::Go => {
                let output = run_with_timeout(
                    Command::new("go")
                        .arg("run")
                        .arg(self.file_name())
                        .current_dir(&dir),
                    COMPILE_TIMEOUT * 2,
                )
                .await;
                output
            }
            Self::Node => {
                run_with_timeout(
                    Command::new("node").arg(self.file_name()).current_dir(&dir),
                    RUN_TIMEOUT,
                )
                .await
            }
            _ => {
                let mut compile = Command::new(self.command());
                match self {
                    Self::Rust => {
                        compile
                            .arg("-O")
                            .arg(self.file_name())
                            .arg("-o")
                            .arg("code");
                    }
                    _ => {
                        compile
                            .arg("-O2")
                            .arg("-o")
                            .arg("code")
                            .arg(self.file_name());
                    }
                }
                let compiled = run_with_timeout(compile.current_dir(&dir), COMPILE_TIMEOUT).await;
                if !compiled.0 || !compiled.1.trim().is_empty() {
                    cleanup(&dir);
                    return compiled;
                }
                run_with_timeout(Command::new("./code").current_dir(&dir), RUN_TIMEOUT).await
            }
        };

        cleanup(&dir);
        result
    }
}

async fn run_with_timeout(command: &mut Command, timeout: Duration) -> (bool, String) {
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            if text.trim().is_empty() {
                text = format!("exit code: {}", output.status.code().unwrap_or(-1));
            }
            (output.status.success(), text)
        }
        Ok(Err(err)) => (false, format!("Failed to start process: {err}")),
        Err(_) => (false, format!("Execution timeout ({}s)", timeout.as_secs())),
    }
}

fn temp_dir() -> PathBuf {
    let name = format!("heroku-eval-{:016x}", rand::thread_rng().gen::<u64>());
    std::env::temp_dir().join(name)
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

inventory::submit! {
    ModuleFactory(|| Box::new(Evaluator))
}
