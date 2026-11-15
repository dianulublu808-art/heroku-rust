// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Safety quiz that unlocks third-party module installation.
//!
//! Mirrors Python's `LoaderRestrictor`: until the owner answers four quiz
//! polls correctly, `.dlmod`, `.dlmall`, `.loadmod` and the inline installer
//! stay locked. The quiz is sent by the inline bot, so votes arrive as raw
//! `UpdateMessagePollVote` updates on the bot's own update stream.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use grammers_client::message::Button;
use grammers_tl_types as tl;
use once_cell::sync::Lazy;
use tokio::sync::Mutex;

use crate::ctx::Ctx;
use crate::tg::Msg;

pub const NS: &str = "LoaderRestrictor";

struct Step {
    key: &'static str,
    /// Zero-based index of the correct answer.
    correct: i32,
}

const QUESTIONS: [Step; 4] = [
    Step {
        key: "q1",
        correct: 3,
    },
    Step {
        key: "q2",
        correct: 1,
    },
    Step {
        key: "q3",
        correct: 2,
    },
    Step {
        key: "q4",
        correct: 3,
    },
];

struct Quiz {
    poll_id: i64,
    message_ids: Vec<i32>,
    step: usize,
}

static QUIZ: Lazy<Mutex<Option<Quiz>>> = Lazy::new(|| Mutex::new(None));

/// Called from the `LoaderRestrictor` module's `on_load` hook.
pub(crate) async fn on_load(ctx: &Arc<Ctx>) -> Result<()> {
    if is_passed(ctx) {
        return Ok(());
    }

    // Python sends the unlock prompt from `inline_ready`. Give the welcome
    // and backup cards a moment to arrive first, then offer the quiz.
    let ctx = ctx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(6)).await;
        if is_passed(&ctx) {
            return;
        }
        let html = ctx.tr("loader_restrictor.unlock_prompt");
        let buttons = vec![vec![Button::data(
            ctx.tr("loader_restrictor.unlock_prompt_btn"),
            b"heroku:lmverify".to_vec(),
        )]];
        let _ = ctx
            .inline
            .send_to_user(&ctx, ctx.me_id, html, buttons)
            .await;
    });
    Ok(())
}

fn is_passed(ctx: &Ctx) -> bool {
    ctx.db.get_bool(NS, "passed", false)
}

/// Returns `true` when module installation must stay blocked, after telling
/// the user how to unlock it.
pub(crate) async fn blocked(ctx: &Arc<Ctx>, message: &Msg) -> bool {
    if is_passed(ctx) {
        return false;
    }
    let Some(username) = ctx.inline.bot_username().await else {
        // Without the inline bot the quiz cannot be taken; stay permissive,
        // exactly like Python's `init_complete` check.
        return false;
    };
    let html = ctx.tf("loader.verify_required", &[&username]);
    let _ = message.reply_html(html).await;
    true
}

/// Inline-callback variant of [`blocked`]. Returns the HTML to render on the
/// gallery message when installation is locked.
pub(crate) async fn blocked_inline(ctx: &Arc<Ctx>) -> Option<String> {
    if is_passed(ctx) {
        return None;
    }
    let username = ctx.inline.bot_username().await?;
    Some(ctx.tf("loader.verify_required", &[&username]))
}

/// Start the quiz from scratch, deleting any previous polls.
pub(crate) async fn start_quiz(ctx: &Arc<Ctx>) -> Result<()> {
    if is_passed(ctx) {
        send_owner(ctx, ctx.tr("loader_restrictor.already_passed")).await?;
        return Ok(());
    }

    if let Some(previous) = QUIZ.lock().await.take() {
        delete_polls(ctx, &previous.message_ids).await;
    }

    let (message_id, poll_id) = send_question(ctx, 0).await?;
    *QUIZ.lock().await = Some(Quiz {
        poll_id,
        message_ids: vec![message_id],
        step: 0,
    });
    spawn_timeout(ctx.clone(), poll_id);
    Ok(())
}

/// Handle a raw `UpdateMessagePollVote` coming from the inline bot.
pub(crate) async fn handle_poll_vote(
    ctx: &Arc<Ctx>,
    vote: &tl::types::UpdateMessagePollVote,
) -> Result<()> {
    if is_passed(ctx) {
        return Ok(());
    }

    let mut guard = QUIZ.lock().await;
    let Some(quiz) = guard.as_mut() else {
        return Ok(());
    };
    if quiz.poll_id != vote.poll_id {
        return Ok(());
    }

    let step = quiz.step;
    let chosen = vote.positions.first().copied().unwrap_or(-1);
    if chosen != QUESTIONS[step].correct {
        let ids = std::mem::take(&mut quiz.message_ids);
        *guard = None;
        drop(guard);
        delete_polls(ctx, &ids).await;
        send_owner(ctx, ctx.tr("loader_restrictor.failed")).await?;
        return Ok(());
    }

    if step + 1 >= QUESTIONS.len() {
        let ids = std::mem::take(&mut quiz.message_ids);
        *guard = None;
        drop(guard);
        ctx.db.set(NS, "passed", true);
        delete_polls(ctx, &ids).await;
        send_owner(ctx, ctx.tr("loader_restrictor.unlocked")).await?;
        return Ok(());
    }

    quiz.step += 1;
    let next = quiz.step;
    drop(guard);

    let (message_id, poll_id) = send_question(ctx, next).await?;
    if let Some(quiz) = QUIZ.lock().await.as_mut() {
        if quiz.step == next {
            quiz.poll_id = poll_id;
            quiz.message_ids.push(message_id);
        }
    }
    spawn_timeout(ctx.clone(), poll_id);
    Ok(())
}

async fn send_question(ctx: &Arc<Ctx>, step: usize) -> Result<(i32, i64)> {
    let key = QUESTIONS[step].key;
    let question = ctx.tr(&format!("loader_restrictor.{key}"));
    let answers: Vec<String> = (1..=4)
        .map(|index| ctx.tr(&format!("loader_restrictor.{key}_ans{index}")))
        .collect();
    ctx.inline
        .send_quiz_poll(ctx.me_id, &question, &answers, QUESTIONS[step].correct)
        .await
}

async fn send_owner(ctx: &Arc<Ctx>, html: String) -> Result<()> {
    if ctx
        .inline
        .send_to_user(ctx, ctx.me_id, html.clone(), Vec::new())
        .await
        .is_ok()
    {
        return Ok(());
    }
    // The inline bot may be down; the userbot can still reach the owner's
    // Saved Messages.
    ctx.send_self(html).await
}

async fn delete_polls(ctx: &Arc<Ctx>, ids: &[i32]) {
    if !ids.is_empty() {
        ctx.inline.delete_bot_messages(ctx.me_id, ids).await;
    }
}

fn spawn_timeout(ctx: Arc<Ctx>, poll_id: i64) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        let mut guard = QUIZ.lock().await;
        if guard.as_ref().map(|quiz| quiz.poll_id) != Some(poll_id) {
            return;
        }
        let ids = guard
            .take()
            .map(|quiz| quiz.message_ids)
            .unwrap_or_default();
        drop(guard);
        delete_polls(&ctx, &ids).await;
        let _ = send_owner(&ctx, ctx.tr("loader_restrictor.timeout")).await;
    });
}
