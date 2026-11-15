// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Message dispatcher: prefix parsing, aliases, security checks, rate limits
//! and watcher/command execution.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::module::{CommandDef, Module, OWNER};
use crate::tg::{escape_html, GrepFilter, Msg};

const RATELIMIT_NS: &str = "heroku.dispatcher";
const MESSAGE_STATES_LIMIT: usize = 1000;

pub struct Dispatcher {
    ratelimit_user: Mutex<HashMap<i64, Counter>>,
    ratelimit_chat: Mutex<HashMap<i64, Counter>>,
    max_user: i64,
    max_chat: i64,
    me_username: Option<String>,
    me_id: i64,
    /// Last processed text hash per `(chat_id, message_id)`. Telegram delivers
    /// reaction changes as message edits with identical text; this lets us
    /// ignore them without running the command again.
    message_states: Mutex<HashMap<(i64, i32), u64>>,
}

#[derive(Clone, Copy)]
struct Counter {
    value: i64,
    updated: Instant,
}

struct Prepared {
    message: Msg,
    args: Args,
    module: Arc<dyn Module>,
    command: CommandDef,
}

impl Dispatcher {
    pub fn new(ctx: &Ctx) -> Self {
        let max_user = ctx.db.get_i64(RATELIMIT_NS, "ratelimit_max_user", 30);
        let max_chat = ctx.db.get_i64(RATELIMIT_NS, "ratelimit_max_chat", 100);
        Self {
            ratelimit_user: Mutex::new(HashMap::new()),
            ratelimit_chat: Mutex::new(HashMap::new()),
            max_user,
            max_chat,
            me_username: ctx.me_username.clone(),
            me_id: ctx.me_id,
            message_states: Mutex::new(HashMap::new()),
        }
    }

    /// Entry point for every incoming `NewMessage` update.
    pub async fn handle(&self, ctx: &Arc<Ctx>, message: Msg) {
        if message.text().trim().is_empty() {
            return;
        }

        if self.chat_blocked(ctx, &message) {
            tracing::debug!("Message is blocklisted in chat {}", message.chat_id());
            return;
        }

        self.remember_state(ctx, &message);
        if self.run_watchers(ctx, &message).await {
            return;
        }

        if let Some(prepared) = self.prepare(ctx, message).await {
            if !self
                .ratelimit_ok(ctx, &prepared.message, prepared.command.ratelimit)
                .await
            {
                return;
            }
            self.execute(ctx, prepared).await;
        }
    }

    /// Entry point for `Update::MessageEdited`. Mirrors the Python dispatcher:
    /// edits are treated as commands (watchers are not re-run), broadcast
    /// channel edits are ignored and reaction-only edits are filtered out.
    pub async fn handle_edit(&self, ctx: &Arc<Ctx>, message: Msg) {
        if message.text().trim().is_empty() {
            return;
        }

        if self.is_reaction_edit(ctx, &message) {
            tracing::debug!(
                "Ignoring reaction edit in {}#{}",
                message.chat_id(),
                message.id()
            );
            return;
        }

        if message.is_broadcast_channel() {
            tracing::debug!("Ignoring edit in broadcast channel {}", message.chat_id());
            return;
        }

        if self.chat_blocked(ctx, &message) {
            tracing::debug!(
                "Edited message is blocklisted in chat {}",
                message.chat_id()
            );
            return;
        }

        self.remember_state(ctx, &message);

        if let Some(prepared) = self.prepare(ctx, message).await {
            if !self
                .ratelimit_ok(ctx, &prepared.message, prepared.command.ratelimit)
                .await
            {
                return;
            }
            self.execute(ctx, prepared).await;
        }
    }

    fn keep_message_state(&self, ctx: &Ctx) -> bool {
        ctx.db.get_bool("heroku.main", "keep_message_state", true)
    }

    fn state_hash(message: &Msg) -> u64 {
        let mut hasher = DefaultHasher::new();
        message.text().hash(&mut hasher);
        hasher.finish()
    }

    fn remember_state(&self, ctx: &Ctx, message: &Msg) {
        if !self.keep_message_state(ctx) {
            if let Ok(mut states) = self.message_states.lock() {
                states.clear();
            }
            return;
        }
        if let Ok(mut states) = self.message_states.lock() {
            states.insert((message.chat_id(), message.id()), Self::state_hash(message));
            while states.len() > MESSAGE_STATES_LIMIT {
                let Some(key) = states.keys().next().copied() else {
                    break;
                };
                states.remove(&key);
            }
        }
    }

    fn is_reaction_edit(&self, ctx: &Ctx, message: &Msg) -> bool {
        if !self.keep_message_state(ctx) {
            return false;
        }
        let Ok(states) = self.message_states.lock() else {
            return false;
        };
        states
            .get(&(message.chat_id(), message.id()))
            .is_some_and(|hash| *hash == Self::state_hash(message))
    }

    fn chat_blocked(&self, ctx: &Ctx, message: &Msg) -> bool {
        let chat_id = message.chat_id();
        let bare_id = crate::modules::common::blacklist_chat_id(chat_id);
        let blacklist = ctx.db.get_value("heroku.main", "blacklist_chats");
        let whitelist = ctx.db.get_value("heroku.main", "whitelist_chats");
        let contains_id = |value: &Value| -> bool {
            match value {
                Value::Array(items) => items.iter().any(|item| {
                    item.as_i64() == Some(chat_id)
                        || item.as_i64() == Some(bare_id)
                        || item.as_str() == Some(&chat_id.to_string())
                        || item.as_str() == Some(&bare_id.to_string())
                }),
                _ => false,
            }
        };
        if blacklist.as_ref().is_some_and(contains_id) {
            return true;
        }
        if let Some(Value::Array(items)) = whitelist {
            if !items.is_empty() && !contains_id(&Value::Array(items)) {
                return true;
            }
        }
        false
    }

    fn module_blocked(&self, ctx: &Ctx, message: &Msg, module: &str) -> bool {
        let chat_id = message.chat_id();
        let bare_id = crate::modules::common::blacklist_chat_id(chat_id);
        let keys = [format!("{chat_id}.{module}"), format!("{bare_id}.{module}")];
        let matches = |keys: &[String], item: &str| keys.iter().any(|key| key == item);
        if let Some(Value::Array(items)) = ctx.db.get_value("heroku.main", "blacklist_chats") {
            if items
                .iter()
                .filter_map(Value::as_str)
                .any(|item| matches(&keys, item))
            {
                return true;
            }
        }
        if let Some(Value::Array(items)) = ctx.db.get_value("heroku.main", "whitelist_modules") {
            if !items.is_empty()
                && !items
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|item| matches(&keys, item))
            {
                return true;
            }
        }
        false
    }

    async fn run_watchers(&self, ctx: &Arc<Ctx>, message: &Msg) -> bool {
        let disabled = ctx.db.get_map("heroku.main", "disabled_watchers");
        let chat_id = message.chat_id();

        for module in ctx.modules.all() {
            let name = module.name();
            if !ctx.module_enabled(name) {
                continue;
            }
            if self.module_blocked(ctx, message, name) {
                continue;
            }
            if let Some(Value::Array(items)) = disabled.get(name) {
                let mut skip = false;
                for item in items {
                    match item {
                        Value::String(value) if value == "*" => skip = true,
                        Value::String(value) if value == "only_chats" && message.is_private() => {
                            skip = true
                        }
                        Value::String(value) if value == "only_pm" && !message.is_private() => {
                            skip = true
                        }
                        Value::String(value) if value == "out" && !message.outgoing() => {
                            skip = true
                        }
                        Value::String(value) if value == "in" && message.outgoing() => skip = true,
                        Value::Number(value) if value.as_i64() == Some(chat_id) => skip = true,
                        Value::String(value) if value.parse::<i64>().ok() == Some(chat_id) => {
                            skip = true
                        }
                        _ => {}
                    }
                }
                if skip {
                    continue;
                }
            }

            match module.on_message(ctx, message).await {
                Ok(true) => return true,
                Ok(false) => {}
                Err(err) => {
                    tracing::error!("Watcher {} failed: {err:#}", module.name());
                }
            }
        }
        false
    }

    async fn prepare(&self, ctx: &Arc<Ctx>, message: Msg) -> Option<Prepared> {
        let sender_id = message.sender_id().unwrap_or(self.me_id);
        let prefixes = ctx.prefixes(sender_id);

        let raw_text = censor(message.text());
        if raw_text.trim().is_empty() {
            return None;
        }

        let prefix = prefixes
            .iter()
            .find(|prefix| !prefix.is_empty() && raw_text.starts_with(prefix.as_str()))?;

        let mut text = raw_text.clone();

        // `.cmd` typed as `..cmd` in outgoing messages: silently strip one prefix.
        if message.outgoing()
            && text.starts_with(&format!("{prefix}{prefix}"))
            && text.chars().count() > prefix.chars().count() * 2
        {
            let candidate = text[prefix.len() * 2..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .split('@')
                .next()
                .unwrap_or_default()
                .to_owned();
            if ctx.modules.command_exists(&candidate) {
                let stripped = text[prefix.len()..].to_owned();
                let _ = message
                    .inner
                    .edit(grammers_client::message::InputMessage::new().text(stripped))
                    .await;
                return None;
            }
        }

        let rest = text.split_off(prefix.len());
        let rest = rest.trim_start();
        if rest.trim().is_empty() {
            return None;
        }

        let (mut token, after_token) = split_first_word(rest);
        let (bare_token, tag) = match token.split_once('@') {
            Some((command, tag)) => (command.to_owned(), Some(tag.to_owned())),
            None => (token.to_owned(), None),
        };
        token = bare_token;

        // `@username` / `@me` suffix rules.
        if let Some(tag) = tag {
            if tag.eq_ignore_ascii_case("me") {
                if !message.outgoing() {
                    return None;
                }
            } else {
                let matches_self = self
                    .me_username
                    .as_deref()
                    .is_some_and(|username| username.eq_ignore_ascii_case(&tag))
                    || tag == self.me_id.to_string();
                if !matches_self {
                    return None;
                }
            }
        } else {
            let mentioned_self = self.me_username.as_deref().is_some_and(|username| {
                message
                    .text()
                    .to_lowercase()
                    .contains(&format!("@{username}").to_lowercase())
            });
            let nickname_required = !message.outgoing()
                && !message.is_private()
                && !mentioned_self
                && !ctx.db.get_bool("heroku.main", "no_nickname", false)
                && !list_contains(
                    &ctx.db.get_value("heroku.main", "nonickcmds"),
                    &token.to_lowercase(),
                )
                && !ctx
                    .db
                    .get_i64_list("heroku.main", "nonickusers")
                    .contains(&sender_id)
                && !ctx
                    .db
                    .get_i64_list("heroku.main", "nonickchats")
                    .contains(&message.chat_id())
                && !ctx
                    .security
                    .has_rule(sender_id, message.chat_id(), "", &token.to_lowercase());
            if nickname_required {
                return None;
            }
        }

        // Alias expansion (`CoreMod.aliases` / `.alias`).
        let direct = ctx.modules.find_command(&token);
        let (command_name, alias_args) = if direct.is_empty() {
            match ctx.apply_alias(&token) {
                Some(expansion) => {
                    let (command, args) = split_first_word(&expansion);
                    if command.is_empty() {
                        return None;
                    }
                    (command, args.trim().to_owned())
                }
                None => (token.clone(), String::new()),
            }
        } else {
            (token.clone(), String::new())
        };

        let mut candidates = ctx.modules.find_command(&command_name);
        if candidates.is_empty() {
            return None;
        }

        // `| grep` / `| grep -v` output filtering. Terminal output is never
        // filtered, matching the behaviour of the Python dispatcher.
        let grep_enabled = ctx.db.get_bool("heroku.main", "grep", false)
            && !candidates
                .iter()
                .any(|(module, _)| module.name() == "TerminalMod");
        let mut after_token_owned = after_token.trim().to_owned();
        let grep_filter = if grep_enabled {
            extract_grep(&mut after_token_owned)
        } else {
            None
        };

        let mut command_text = command_name.clone();
        if !alias_args.is_empty() {
            command_text.push(' ');
            command_text.push_str(&alias_args);
        }
        if !after_token_owned.is_empty() {
            if !alias_args.is_empty() {
                command_text.push(' ');
            }
            command_text.push_str(&after_token_owned);
        }

        // Rewrite the message text to the canonical form (handlers that read
        // `message.text()` expect the command name, not its alias).
        let rewritten = format!("{prefix}{command_text}");
        let mut message = message;
        message.set_text(rewritten);
        if let Some(filter) = grep_filter {
            message.set_grep(filter);
        }

        let args = Args::parse(if alias_args.is_empty() {
            after_token_owned
        } else if after_token_owned.is_empty() {
            alias_args.clone()
        } else {
            format!("{alias_args} {after_token_owned}")
        });

        candidates.retain(|(module, command)| {
            ctx.command_enabled(module.name(), command.name)
                && !self.module_blocked(ctx, &message, module.name())
        });

        if candidates.is_empty() {
            return None;
        }
        if candidates.len() > 1 {
            tracing::warn!(
                "Command conflict for {:?}: {} handlers; using the last one",
                command_name,
                candidates.len()
            );
        }

        let mut allowed = Vec::new();
        for (module, command) in candidates {
            if ctx
                .security
                .check(ctx, &message, module.name(), command.name, command.security)
                .await
            {
                allowed.push((module, command));
            }
        }

        let (module, command) = allowed.pop()?;
        Some(Prepared {
            message,
            args,
            module,
            command,
        })
    }

    async fn execute(&self, ctx: &Arc<Ctx>, prepared: Prepared) {
        let Prepared {
            message,
            args,
            module,
            command,
        } = prepared;

        tracing::debug!(
            "Dispatching .{} from {} in {}",
            command.name,
            message.sender_id().unwrap_or_default(),
            message.chat_id()
        );

        let name = command.name;
        if let Err(err) = module.handle(ctx, name, &message, &args).await {
            tracing::error!("Command .{name} failed: {err:#}");
            let header = ctx.tf("errors.command_failed", &[&escape_html(message.text())]);
            let text = if ctx.db.get_bool("heroku.main", "inlinelogs", true) {
                format!(
                    "{header}\n\n{label}\n<pre><code class=\"language-logs\">{}</code></pre>",
                    escape_html(&format!("{err:#}")),
                    label = ctx.tr("errors.error")
                )
            } else {
                header
            };
            let _ = message.reply_html(text).await;
        }
    }

    async fn ratelimit_ok(&self, ctx: &Arc<Ctx>, message: &Msg, ratelimit: bool) -> bool {
        if ctx.security.check(ctx, message, "", "", OWNER).await {
            return true;
        }

        let sender_id = match message.sender_id() {
            Some(sender_id) => sender_id,
            None => return true,
        };
        let chat_id = message.chat_id();

        let chat = self.current_chat(chat_id);
        let user = self.current_user(sender_id);

        let severity = (if ratelimit { 5 } else { 2 }) * ((user + chat) / 30 + 1);
        let user = user + severity;
        let chat = chat + severity;

        let user_ok = if let Ok(mut map) = self.ratelimit_user.lock() {
            map.insert(
                sender_id,
                Counter {
                    value: user,
                    updated: Instant::now(),
                },
            );
            user <= self.max_user
        } else {
            true
        };
        let chat_ok = if let Ok(mut map) = self.ratelimit_chat.lock() {
            map.insert(
                chat_id,
                Counter {
                    value: chat,
                    updated: Instant::now(),
                },
            );
            chat <= self.max_chat
        } else {
            true
        };

        user_ok && chat_ok
    }

    fn current_user(&self, user_id: i64) -> i64 {
        self.decayed(&self.ratelimit_user, user_id, self.max_user)
    }

    fn current_chat(&self, chat_id: i64) -> i64 {
        self.decayed(&self.ratelimit_chat, chat_id, self.max_chat)
    }

    fn decayed(&self, storage: &Mutex<HashMap<i64, Counter>>, key: i64, max: i64) -> i64 {
        let Ok(map) = storage.lock() else {
            return 0;
        };
        let Some(counter) = map.get(&key) else {
            return 0;
        };
        let elapsed = counter.updated.elapsed().as_secs() as i64;
        let decay = elapsed / max.max(1);
        (counter.value - decay).max(0)
    }

    /// Drop stale rate-limit entries. Called periodically from the main loop.
    pub fn tick_ratelimit(&self) {
        let now = Instant::now();
        if let Ok(mut map) = self.ratelimit_user.lock() {
            map.retain(|_, counter| {
                counter.value > now.duration_since(counter.updated).as_secs() as i64 * 2
            });
        }
        if let Ok(mut map) = self.ratelimit_chat.lock() {
            map.retain(|_, counter| {
                counter.value > now.duration_since(counter.updated).as_secs() as i64 * 2
            });
        }
    }
}

/// Parse and strip a trailing `| grep pattern` / `| grep -v pattern` clause.
/// Returns `None` when no filter is present; `||grep` escapes filtering and is
/// rewritten to a literal `| grep`.
fn extract_grep(text: &mut String) -> Option<GrepFilter> {
    if text.contains("||grep") || text.contains("|| grep") {
        *text = text
            .replace("||grep", "| grep")
            .replace("|| grep", "| grep");
        return None;
    }

    let mut marker = None;
    for needle in ["| grep ", "|grep "] {
        if let Some(index) = text.rfind(needle) {
            if marker.is_none_or(|current| index > current) {
                marker = Some(index);
            }
        }
    }
    let index = marker?;

    let rest = text[index..]
        .trim_start_matches('|')
        .trim_start()
        .trim_start_matches("grep")
        .trim()
        .to_owned();
    if rest.is_empty() {
        return None;
    }
    text.truncate(index);
    *text = text.trim_end().to_owned();

    let mut include = Some(rest.clone());
    let mut exclude = None;
    if let Some(pattern) = rest.strip_prefix("-v ") {
        include = None;
        exclude = Some(pattern.trim().to_owned());
    } else if let Some((before, after)) = rest.split_once(" -v ") {
        include = Some(before.trim().to_owned());
        exclude = Some(after.trim().to_owned());
    }

    Some(GrepFilter { include, exclude })
}

fn split_first_word(text: &str) -> (String, &str) {
    let text = text.trim_start();
    match text.find(char::is_whitespace) {
        Some(index) => (text[..index].to_owned(), text[index..].trim_start()),
        None => (text.to_owned(), ""),
    }
}

fn list_contains(value: &Option<Value>, needle: &str) -> bool {
    value
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| {
            items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|item| item.eq_ignore_ascii_case(needle))
            })
        })
        .unwrap_or(false)
}

/// Strip invisible direction/format characters that Telegram clients add to
/// messages. The original userbot calls this `censor`.
pub fn censor(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                ch,
                '\u{200b}'
                    | '\u{200c}'
                    | '\u{200d}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{feff}'
            )
        })
        .collect()
}

#[cfg(test)]
mod grep_tests {
    use super::extract_grep;

    #[test]
    fn parses_include_filter() {
        let mut text = ".help | grep security".to_owned();
        let filter = extract_grep(&mut text).expect("filter");
        assert_eq!(text, ".help");
        assert_eq!(filter.include.as_deref(), Some("security"));
        assert!(filter.exclude.is_none());
    }

    #[test]
    fn parses_include_and_exclude() {
        let mut text = ".help |grep mask -v owner".to_owned();
        let filter = extract_grep(&mut text).expect("filter");
        assert_eq!(text, ".help");
        assert_eq!(filter.include.as_deref(), Some("mask"));
        assert_eq!(filter.exclude.as_deref(), Some("owner"));
    }

    #[test]
    fn parses_exclude_only() {
        let mut text = ".help | grep -v owner".to_owned();
        let filter = extract_grep(&mut text).expect("filter");
        assert_eq!(text, ".help");
        assert!(filter.include.is_none());
        assert_eq!(filter.exclude.as_deref(), Some("owner"));
    }

    #[test]
    fn double_pipe_escapes_filter() {
        let mut text = ".help ||grep foo".to_owned();
        assert!(extract_grep(&mut text).is_none());
        assert_eq!(text, ".help | grep foo");
    }
}
