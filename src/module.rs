// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Module and command metadata, registry and the `inventory`-based built-in
//! plugin discovery.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

use crate::args::Args;
use crate::ctx::Ctx;
use crate::tg::Msg;

// Security flags. They match the bitmask used by Heroku/Hikka, so existing
// database values keep working.
pub const OWNER: u32 = 1 << 0;
pub const SUDO: u32 = 1 << 1;
pub const SUPPORT: u32 = 1 << 2;
pub const GROUP_OWNER: u32 = 1 << 3;
pub const GROUP_ADMIN_ADD_ADMINS: u32 = 1 << 4;
pub const GROUP_ADMIN_CHANGE_INFO: u32 = 1 << 5;
pub const GROUP_ADMIN_BAN_USERS: u32 = 1 << 6;
pub const GROUP_ADMIN_DELETE_MESSAGES: u32 = 1 << 7;
pub const GROUP_ADMIN_PIN_MESSAGES: u32 = 1 << 8;
pub const GROUP_ADMIN_INVITE_USERS: u32 = 1 << 9;
pub const GROUP_ADMIN: u32 = 1 << 10;
pub const GROUP_MEMBER: u32 = 1 << 11;
pub const PM: u32 = 1 << 12;
pub const EVERYONE: u32 = 1 << 13;

pub const GROUP_ADMIN_ANY: u32 = GROUP_ADMIN_ADD_ADMINS
    | GROUP_ADMIN_CHANGE_INFO
    | GROUP_ADMIN_BAN_USERS
    | GROUP_ADMIN_DELETE_MESSAGES
    | GROUP_ADMIN_PIN_MESSAGES
    | GROUP_ADMIN_INVITE_USERS
    | GROUP_ADMIN;
/// Version of the module-facing API. Bumped when a source-incompatible change
/// is made to the [`Module`] trait or its data types. Modules may declare
/// [`Module::api_version`] and are warned at load time when they target a newer
/// API than the host provides.
pub const MODULE_API_VERSION: u32 = 2;

pub const DEFAULT_PERMISSIONS: u32 = OWNER;
pub const ALL: u32 = (1 << 14) - 1;

pub const PERMISSION_NAMES: &[(&str, u32)] = &[
    ("owner", OWNER),
    ("sudo", SUDO),
    ("support", SUPPORT),
    ("group_owner", GROUP_OWNER),
    ("group_admin_add_admins", GROUP_ADMIN_ADD_ADMINS),
    ("group_admin_change_info", GROUP_ADMIN_CHANGE_INFO),
    ("group_admin_ban_users", GROUP_ADMIN_BAN_USERS),
    ("group_admin_delete_messages", GROUP_ADMIN_DELETE_MESSAGES),
    ("group_admin_pin_messages", GROUP_ADMIN_PIN_MESSAGES),
    ("group_admin_invite_users", GROUP_ADMIN_INVITE_USERS),
    ("group_admin", GROUP_ADMIN),
    ("group_member", GROUP_MEMBER),
    ("pm", PM),
    ("everyone", EVERYONE),
];

pub fn permission_bit(name: &str) -> Option<u32> {
    let name = name.trim().to_lowercase().replace('-', "_");
    PERMISSION_NAMES
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, bit)| *bit)
}

#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
    pub security: u32,
    pub ratelimit: bool,
}

impl CommandDef {
    pub const fn new(name: &'static str, description: &'static str) -> Self {
        Self {
            name,
            aliases: &[],
            description,
            usage: "",
            security: 0,
            ratelimit: false,
        }
    }

    pub const fn aliases(mut self, aliases: &'static [&'static str]) -> Self {
        self.aliases = aliases;
        self
    }

    pub const fn usage(mut self, usage: &'static str) -> Self {
        self.usage = usage;
        self
    }

    pub const fn security(mut self, security: u32) -> Self {
        self.security = security;
        self
    }

    pub const fn ratelimit(mut self) -> Self {
        self.ratelimit = true;
        self
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigDefault {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(&'static str),
    List(&'static [&'static str]),
}

impl ConfigDefault {
    pub fn to_value(self) -> Value {
        match self {
            Self::Bool(value) => Value::Bool(value),
            Self::Int(value) => Value::from(value),
            Self::Float(value) => Value::from(value),
            Self::Text(value) => Value::String(value.to_owned()),
            Self::List(values) => Value::Array(
                values
                    .iter()
                    .map(|v| Value::String((*v).to_owned()))
                    .collect(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ConfigKind {
    Bool,
    Int,
    Float,
    Text,
    Choice(&'static [&'static str]),
    List,
    Hidden,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfigSpec {
    pub key: &'static str,
    pub description: &'static str,
    pub default: ConfigDefault,
    pub kind: ConfigKind,
    pub folder: Option<&'static str>,
}

impl ConfigSpec {
    pub const fn new(
        key: &'static str,
        description: &'static str,
        default: ConfigDefault,
        kind: ConfigKind,
    ) -> Self {
        Self {
            key,
            description,
            default,
            kind,
            folder: None,
        }
    }

    pub const fn folder(mut self, folder: &'static str) -> Self {
        self.folder = Some(folder);
        self
    }
}

/// Declarative filters for a [`WatcherDef`].
///
/// All fields are optional and combine with logical AND, matching Heroku's
/// watcher decorators. Media filters use [`Msg::media_kind`]. A watcher with
/// `editable: Some(true)` also receives message edits; every other watcher only
/// sees new messages.
#[derive(Clone, Debug, Default)]
pub struct WatcherFilter {
    /// Only messages that are not commands.
    pub no_commands: bool,
    /// Only messages that look like commands.
    pub only_commands: bool,
    /// `Some(true)` — outgoing only, `Some(false)` — incoming only.
    pub outgoing: Option<bool>,
    /// `Some(true)` — also handle edits, `Some(false)` — new messages only.
    pub editable: Option<bool>,
    pub no_media: bool,
    pub only_media: bool,
    pub only_photos: bool,
    pub only_videos: bool,
    pub only_audios: bool,
    pub only_docs: bool,
    pub only_stickers: bool,
    pub no_photos: bool,
    pub no_videos: bool,
    pub no_audios: bool,
    pub no_docs: bool,
    pub no_stickers: bool,
    pub only_channels: bool,
    pub only_groups: bool,
    pub only_pm: bool,
    pub no_pm: bool,
    pub no_channels: bool,
    pub no_groups: bool,
    pub no_forwards: bool,
    pub only_forwards: bool,
    pub no_reply: bool,
    pub only_reply: bool,
    pub no_mention: bool,
    pub mention: bool,
    pub startswith: Option<String>,
    pub endswith: Option<String>,
    pub contains: Option<String>,
    pub regex: Option<String>,
    pub from_id: Option<i64>,
    pub chat_id: Option<i64>,
}

/// Upper bound for the watcher regex cache. Modules call `watchers()` on every
/// update, so compiling the same pattern per message would be wasteful.
const REGEX_CACHE_LIMIT: usize = 256;

static WATCHER_REGEX_CACHE: Lazy<Mutex<HashMap<String, Option<Regex>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Return a compiled watcher regex, reusing the previous compilation.
///
/// `None` is cached for invalid patterns too, so a broken module cannot make
/// the dispatcher retry (and log) compilation on every message. A poisoned
/// lock falls back to direct compilation instead of panicking.
fn cached_regex(pattern: &str) -> Option<Regex> {
    match WATCHER_REGEX_CACHE.lock() {
        Ok(mut cache) => {
            if let Some(compiled) = cache.get(pattern) {
                return compiled.clone();
            }
            let compiled = Regex::new(pattern).ok();
            if cache.len() >= REGEX_CACHE_LIMIT {
                // Evict one arbitrary entry instead of clearing the whole
                // cache, so a module with many patterns does not thrash it.
                if let Some(oldest) = cache.keys().next().cloned() {
                    cache.remove(&oldest);
                }
            }
            cache.insert(pattern.to_owned(), compiled.clone());
            compiled
        }
        Err(_) => Regex::new(pattern).ok(),
    }
}

impl WatcherFilter {
    /// Text-only part of the filter. Exposed for unit tests and for modules
    /// that already have a message-like value.
    pub fn matches_text(&self, text: &str) -> bool {
        if let Some(pattern) = &self.startswith {
            if !text.starts_with(pattern) {
                return false;
            }
        }
        if let Some(pattern) = &self.endswith {
            if !text.ends_with(pattern) {
                return false;
            }
        }
        if let Some(pattern) = &self.contains {
            if !text.contains(pattern) {
                return false;
            }
        }
        if let Some(pattern) = &self.regex {
            if !cached_regex(pattern).is_some_and(|regex| regex.is_match(text)) {
                return false;
            }
        }
        true
    }

    /// Media part of the filter. Kept separate so it can be tested without a
    /// real Telegram update.
    pub fn matches_media(&self, media: crate::tg::MediaKind) -> bool {
        use crate::tg::MediaKind;

        if self.no_media && media.is_media() {
            return false;
        }
        if self.only_media && !media.is_media() {
            return false;
        }
        // Positive selectors are exact matches. `only_docs` intentionally also
        // accepts videos and audio files, because Telegram stores them as
        // documents.
        if self.only_photos && media != MediaKind::Photo {
            return false;
        }
        if self.only_videos && media != MediaKind::Video {
            return false;
        }
        if self.only_audios && media != MediaKind::Audio {
            return false;
        }
        if self.only_stickers && media != MediaKind::Sticker {
            return false;
        }
        if self.only_docs
            && !matches!(
                media,
                MediaKind::Document | MediaKind::Video | MediaKind::Audio
            )
        {
            return false;
        }

        // Negative selectors mirror the Python `no_*` tags.
        if self.no_photos && media == MediaKind::Photo {
            return false;
        }
        if self.no_videos && media == MediaKind::Video {
            return false;
        }
        if self.no_audios && media == MediaKind::Audio {
            return false;
        }
        if self.no_stickers && media == MediaKind::Sticker {
            return false;
        }
        // Mirror `only_docs`: Telegram stores videos and audio files as
        // documents, so `no_docs` excludes the same set.
        if self.no_docs
            && matches!(
                media,
                MediaKind::Document | MediaKind::Video | MediaKind::Audio
            )
        {
            return false;
        }
        true
    }

    /// Evaluate the complete filter against a real update.
    pub fn matches(&self, ctx: &Ctx, message: &Msg, edited: bool) -> bool {
        if edited && self.editable != Some(true) {
            return false;
        }

        if let Some(expected) = self.outgoing {
            if message.outgoing() != expected {
                return false;
            }
        }

        let private = message.is_private();
        let group = message.is_group();
        let channel = message.is_channel();
        if self.only_pm && !private {
            return false;
        }
        if self.no_pm && private {
            return false;
        }
        if self.only_groups && !group {
            return false;
        }
        if self.no_groups && group {
            return false;
        }
        if self.only_channels && !channel {
            return false;
        }
        if self.no_channels && channel {
            return false;
        }

        if !self.matches_media(message.media_kind()) {
            return false;
        }

        if self.no_forwards && message.is_forwarded() {
            return false;
        }
        if self.only_forwards && !message.is_forwarded() {
            return false;
        }
        if self.no_reply && message.has_reply() {
            return false;
        }
        if self.only_reply && !message.has_reply() {
            return false;
        }
        if self.mention && !message.mentioned() {
            return false;
        }
        if self.no_mention && message.mentioned() {
            return false;
        }
        if let Some(expected) = self.from_id {
            if message.sender_id() != Some(expected) {
                return false;
            }
        }
        if let Some(expected) = self.chat_id {
            if message.chat_id() != expected {
                return false;
            }
        }

        let text = message.text();
        if self.no_commands && is_command_text(ctx, message, text) {
            return false;
        }
        if self.only_commands && !is_command_text(ctx, message, text) {
            return false;
        }
        self.matches_text(text)
    }
}

fn is_command_text(ctx: &Ctx, message: &Msg, text: &str) -> bool {
    let sender = message.sender_id().unwrap_or(ctx.me_id);
    let text = text.trim_start();
    ctx.prefixes(sender)
        .iter()
        .any(|prefix| !prefix.is_empty() && text.starts_with(prefix.as_str()))
}

/// A named watcher handled by [`Module::on_watcher`].
#[derive(Clone, Debug)]
pub struct WatcherDef {
    pub name: &'static str,
    pub filter: WatcherFilter,
}

impl WatcherDef {
    pub const fn new(name: &'static str, filter: WatcherFilter) -> Self {
        Self { name, filter }
    }
}

#[async_trait]
pub trait Module: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str {
        ""
    }
    fn author(&self) -> &'static str {
        "Heroku-userbot"
    }
    fn version(&self) -> &'static str {
        crate::config::VERSION
    }
    fn commands(&self) -> Vec<CommandDef> {
        Vec::new()
    }
    fn config_spec(&self) -> Vec<ConfigSpec> {
        Vec::new()
    }
    fn core(&self) -> bool {
        true
    }

    /// Version of the module API this module was written against.
    fn api_version(&self) -> u32 {
        MODULE_API_VERSION
    }

    /// Module names that must be loaded first. Names are matched
    /// case-insensitively and the `Mod` suffix is optional.
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }

    /// Legacy watcher: see [`Module::on_message`]. Used by
    /// `.watchers` / `.watcher`, since Rust has no runtime reflection.
    fn has_watcher(&self) -> bool {
        false
    }

    /// Declarative watchers with filters. Prefer this over [`Module::on_message`]
    /// for new modules.
    fn watchers(&self) -> Vec<WatcherDef> {
        Vec::new()
    }

    /// Whether the module has any watcher, legacy or declarative.
    fn has_any_watcher(&self) -> bool {
        self.has_watcher() || !self.watchers().is_empty()
    }

    /// Called for each matching [`WatcherDef`]. Return `true` to stop dispatch
    /// of this update to other watchers and commands.
    async fn on_watcher(
        &self,
        _ctx: &Arc<Ctx>,
        _watcher: &str,
        _message: &Msg,
        _edited: bool,
    ) -> Result<bool> {
        Ok(false)
    }

    /// Called when a command, watcher, callback or lifecycle hook fails.
    /// Errors from this hook are logged and never propagate.
    async fn on_error(&self, _ctx: &Arc<Ctx>, _source: &str, _error: &anyhow::Error) {}

    /// Whether the module handles its own inline callback buttons. Used by
    /// `.help`/`.watchers`-style introspection, since Rust has no reflection.
    fn has_callback(&self) -> bool {
        false
    }

    /// Whether `sender` may press this module's buttons. Defaults to the gate
    /// Heroku applies to module callback handlers (owner/sudo only). Override
    /// this for buttons that are intentionally public.
    fn callback_allowed(&self, ctx: &Arc<Ctx>, sender: i64) -> bool {
        ctx.security.is_owner(sender) || ctx.security.is_sudo(sender)
    }

    /// Handle a callback button pressed on a message produced by this module.
    /// Return `true` if the callback was consumed. The inline bot calls every
    /// module that reports [`Module::has_callback`] for unknown `heroku:*`
    /// payloads.
    async fn on_callback(
        &self,
        _ctx: &Arc<Ctx>,
        _call: &grammers_client::update::CallbackQuery,
        _data: &str,
    ) -> Result<bool> {
        Ok(false)
    }

    async fn on_load(&self, _ctx: &Arc<Ctx>) -> Result<()> {
        Ok(())
    }

    async fn on_unload(&self, _ctx: &Arc<Ctx>) -> Result<()> {
        Ok(())
    }

    /// Passive watcher. Return `true` if the message was handled and command
    /// dispatch should stop.
    async fn on_message(&self, _ctx: &Arc<Ctx>, _message: &Msg) -> Result<bool> {
        Ok(false)
    }

    async fn handle(&self, ctx: &Arc<Ctx>, command: &str, message: &Msg, args: &Args)
        -> Result<()>;
}

fn module_name_matches(candidate: &str, wanted: &str) -> bool {
    candidate.eq_ignore_ascii_case(wanted)
        || candidate
            .strip_suffix("Mod")
            .is_some_and(|short| short.eq_ignore_ascii_case(wanted))
        || wanted
            .strip_suffix("Mod")
            .is_some_and(|short| candidate.eq_ignore_ascii_case(short))
}

/// Stable topological ordering: independent modules stay alphabetical, but a
/// module always appears after its declared dependencies. Missing dependencies
/// are ignored (documented modules may be optional) and cycles fall back to
/// name order instead of dropping modules.
pub(crate) fn order_by_dependencies(mut modules: Vec<Arc<dyn Module>>) -> Vec<Arc<dyn Module>> {
    modules.sort_by(|left, right| left.name().cmp(right.name()));
    let known: Vec<String> = modules
        .iter()
        .map(|module| module.name().to_owned())
        .collect();
    let mut ordered: Vec<Arc<dyn Module>> = Vec::with_capacity(modules.len());
    let mut pending = modules;

    while !pending.is_empty() {
        let mut index = 0;
        let mut progressed = false;
        while index < pending.len() {
            let ready = pending[index].dependencies().iter().all(|dependency| {
                let exists = known
                    .iter()
                    .any(|name| module_name_matches(name, dependency));
                !exists
                    || ordered
                        .iter()
                        .any(|module| module_name_matches(module.name(), dependency))
            });
            if ready {
                ordered.push(pending.remove(index));
                progressed = true;
            } else {
                index += 1;
            }
        }
        if !progressed {
            let stuck: Vec<&str> = pending.iter().map(|module| module.name()).collect();
            tracing::warn!(
                modules = ?stuck,
                "Circular module dependencies; loading the rest by name"
            );
            ordered.append(&mut pending);
        }
    }
    ordered
}

pub struct ModuleFactory(pub fn() -> Box<dyn Module>);

inventory::collect!(ModuleFactory);

pub struct Modules {
    modules: Vec<Arc<dyn Module>>,
    by_name: HashMap<String, usize>,
    commands: HashMap<String, Vec<usize>>,
}

impl Modules {
    pub fn discover() -> Self {
        let modules: Vec<Arc<dyn Module>> = inventory::iter::<ModuleFactory>
            .into_iter()
            .map(|factory| Arc::from((factory.0)()) as Arc<dyn Module>)
            .collect();
        let modules = order_by_dependencies(modules);

        let mut by_name = HashMap::new();
        let mut commands: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, module) in modules.iter().enumerate() {
            by_name.insert(module.name().to_lowercase(), index);
            let short = module
                .name()
                .strip_suffix("Mod")
                .unwrap_or(module.name())
                .to_lowercase();
            by_name.entry(short).or_insert(index);

            for command in module.commands() {
                commands
                    .entry(command.name.to_lowercase())
                    .or_default()
                    .push(index);
                for alias in command.aliases {
                    commands
                        .entry((*alias).to_lowercase())
                        .or_default()
                        .push(index);
                }
            }
        }

        Self {
            modules,
            by_name,
            commands,
        }
    }

    pub fn all(&self) -> &[Arc<dyn Module>] {
        &self.modules
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Module>> {
        let name = name.trim().trim_start_matches('.').to_lowercase();
        self.by_name
            .get(&name)
            .and_then(|index| self.modules.get(*index))
            .cloned()
    }

    /// All commands (including aliases) with the module that owns them.
    pub fn find_command(&self, name: &str) -> Vec<(Arc<dyn Module>, CommandDef)> {
        let name = name.trim().trim_start_matches('.').to_lowercase();
        let mut out = Vec::new();
        if let Some(indexes) = self.commands.get(&name) {
            for index in indexes {
                let module = self.modules[*index].clone();
                if let Some(command) = module.commands().into_iter().find(|command| {
                    command.name.eq_ignore_ascii_case(&name)
                        || command
                            .aliases
                            .iter()
                            .any(|alias| alias.eq_ignore_ascii_case(&name))
                }) {
                    out.push((module, command));
                }
            }
        }
        out
    }

    pub fn command_exists(&self, name: &str) -> bool {
        self.commands
            .contains_key(&name.trim().trim_start_matches('.').to_lowercase())
    }

    pub fn commands_of(&self, module: &str) -> Vec<CommandDef> {
        self.get(module)
            .map(|module| module.commands())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_bits_match_heroku() {
        assert_eq!(permission_bit("owner"), Some(1));
        assert_eq!(permission_bit("sudo"), Some(2));
        assert_eq!(permission_bit("Group-Admin"), Some(1 << 10));
        assert_eq!(permission_bit("everyone"), Some(1 << 13));
        assert_eq!(permission_bit("nonsense"), None);
    }

    #[test]
    fn every_known_command_is_discoverable() {
        let modules = Modules::discover();
        assert!(!modules.all().is_empty());
        for module in modules.all() {
            for command in module.commands() {
                assert!(
                    modules.command_exists(command.name),
                    "missing command {} from {}",
                    command.name,
                    module.name()
                );
                for alias in command.aliases {
                    assert!(
                        modules.command_exists(alias),
                        "missing alias {alias} from {}",
                        module.name()
                    );
                }
            }
        }
    }

    #[test]
    fn module_lookup_is_case_insensitive_and_short() {
        let modules = Modules::discover();
        assert!(modules.get("help").is_some());
        assert!(modules.get("Help").is_some());
        assert!(modules.get("CoreMod").is_some());
        assert!(modules.get("core").is_some());
        assert!(modules.get("nope").is_none());
    }

    #[test]
    fn watcher_text_filters_combine() {
        let filter = WatcherFilter {
            startswith: Some(".test".to_owned()),
            contains: Some("needle".to_owned()),
            endswith: Some("!".to_owned()),
            ..Default::default()
        };
        assert!(filter.matches_text(".test a needle b!"));
        assert!(!filter.matches_text(".other needle!"));
        assert!(!filter.matches_text(".test needle"));

        let regex = WatcherFilter {
            regex: Some(r"^\d{3}-\d{2}$".to_owned()),
            ..Default::default()
        };
        assert!(regex.matches_text("123-45"));
        assert!(!regex.matches_text("123-456"));

        let broken = WatcherFilter {
            regex: Some("(".to_owned()),
            ..Default::default()
        };
        assert!(!broken.matches_text("anything"));
    }

    #[test]
    fn watcher_regex_cache_is_bounded_and_reuses_compiled_patterns() {
        let pattern = "^cache-[0-9]+$";
        for _ in 0..5 {
            let regex = cached_regex(pattern).expect("valid regex");
            assert!(regex.is_match("cache-42"));
        }
        assert!(cached_regex("(").is_none());
        assert!(cached_regex("(").is_none());

        for index in 0..(REGEX_CACHE_LIMIT + 50) {
            assert!(cached_regex(&format!("^bounded-{index}$")).is_some());
        }
        let len = WATCHER_REGEX_CACHE
            .lock()
            .map(|cache| cache.len())
            .unwrap_or_default();
        assert!(
            len <= REGEX_CACHE_LIMIT,
            "regex cache grew past its limit: {len}"
        );
    }

    #[test]
    fn watcher_media_filters_include_and_exclude() {
        use crate::tg::MediaKind;

        let only_photos = WatcherFilter {
            only_photos: true,
            ..Default::default()
        };
        assert!(only_photos.matches_media(MediaKind::Photo));
        assert!(!only_photos.matches_media(MediaKind::Video));
        assert!(!only_photos.matches_media(MediaKind::None));

        let documents = WatcherFilter {
            only_docs: true,
            ..Default::default()
        };
        assert!(documents.matches_media(MediaKind::Document));
        assert!(documents.matches_media(MediaKind::Video));
        assert!(documents.matches_media(MediaKind::Audio));
        assert!(!documents.matches_media(MediaKind::Photo));

        let no_photos = WatcherFilter {
            no_photos: true,
            ..Default::default()
        };
        assert!(!no_photos.matches_media(MediaKind::Photo));
        assert!(no_photos.matches_media(MediaKind::Video));
        assert!(no_photos.matches_media(MediaKind::None));

        let no_docs = WatcherFilter {
            no_docs: true,
            ..Default::default()
        };
        assert!(!no_docs.matches_media(MediaKind::Document));
        assert!(!no_docs.matches_media(MediaKind::Video));
        assert!(!no_docs.matches_media(MediaKind::Audio));
        assert!(no_docs.matches_media(MediaKind::Photo));
        assert!(no_docs.matches_media(MediaKind::Sticker));
        assert!(no_docs.matches_media(MediaKind::None));

        // `only_docs` and `no_docs` describe the same document set.
        let contradictory = WatcherFilter {
            only_docs: true,
            no_docs: true,
            ..Default::default()
        };
        for media in [
            MediaKind::Document,
            MediaKind::Video,
            MediaKind::Audio,
            MediaKind::Photo,
        ] {
            assert!(!contradictory.matches_media(media));
        }

        let text_only = WatcherFilter {
            no_media: true,
            ..Default::default()
        };
        assert!(text_only.matches_media(MediaKind::None));
        assert!(!text_only.matches_media(MediaKind::Sticker));

        let media_only = WatcherFilter {
            only_media: true,
            ..Default::default()
        };
        assert!(!media_only.matches_media(MediaKind::None));
        assert!(media_only.matches_media(MediaKind::Document));
    }

    struct Dummy {
        name: &'static str,
        deps: &'static [&'static str],
    }

    #[async_trait]
    impl Module for Dummy {
        fn name(&self) -> &'static str {
            self.name
        }

        fn dependencies(&self) -> &'static [&'static str] {
            self.deps
        }

        async fn handle(
            &self,
            _ctx: &Arc<Ctx>,
            _command: &str,
            _message: &Msg,
            _args: &Args,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn dependencies_are_loaded_first() {
        let modules: Vec<Arc<dyn Module>> = vec![
            Arc::new(Dummy {
                name: "ChildMod",
                deps: &["Base"],
            }),
            Arc::new(Dummy {
                name: "Base",
                deps: &[],
            }),
            Arc::new(Dummy {
                name: "AlsoMod",
                deps: &["childmod"],
            }),
            Arc::new(Dummy {
                name: "MissingDepMod",
                deps: &["does-not-exist"],
            }),
        ];
        let ordered = order_by_dependencies(modules);
        let names: Vec<&str> = ordered.iter().map(|module| module.name()).collect();
        let position = |name: &str| names.iter().position(|item| *item == name).expect(name);

        assert!(position("Base") < position("ChildMod"));
        assert!(position("ChildMod") < position("AlsoMod"));
        assert!(names.contains(&"MissingDepMod"));
    }

    struct WatcherModule {
        legacy: bool,
        filtered: bool,
    }

    #[async_trait]
    impl Module for WatcherModule {
        fn name(&self) -> &'static str {
            "WatcherModule"
        }

        fn has_watcher(&self) -> bool {
            self.legacy
        }

        fn watchers(&self) -> Vec<WatcherDef> {
            if self.filtered {
                vec![WatcherDef::new("test", WatcherFilter::default())]
            } else {
                Vec::new()
            }
        }

        async fn handle(
            &self,
            _ctx: &Arc<Ctx>,
            _command: &str,
            _message: &Msg,
            _args: &Args,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn watcher_introspection_covers_legacy_and_filtered_apis() {
        let none = WatcherModule {
            legacy: false,
            filtered: false,
        };
        let legacy = WatcherModule {
            legacy: true,
            filtered: false,
        };
        let filtered = WatcherModule {
            legacy: false,
            filtered: true,
        };
        assert!(!none.has_any_watcher());
        assert!(legacy.has_any_watcher());
        assert!(filtered.has_any_watcher());
        assert_eq!(none.api_version(), MODULE_API_VERSION);
    }

    #[test]
    fn cycles_keep_every_module_and_fall_back_to_name_order() {
        let modules: Vec<Arc<dyn Module>> = vec![
            Arc::new(Dummy {
                name: "AlphaMod",
                deps: &["Beta"],
            }),
            Arc::new(Dummy {
                name: "BetaMod",
                deps: &["Alpha"],
            }),
            Arc::new(Dummy {
                name: "IndependentMod",
                deps: &[],
            }),
        ];
        let ordered = order_by_dependencies(modules);
        let names: Vec<&str> = ordered.iter().map(|module| module.name()).collect();

        // No module is dropped, and the independent module is still loaded
        // before the cycle fallback appends the rest.
        assert_eq!(names, vec!["IndependentMod", "AlphaMod", "BetaMod"]);
    }

    #[test]
    fn a_self_dependency_does_not_drop_the_module() {
        let modules: Vec<Arc<dyn Module>> = vec![
            Arc::new(Dummy {
                name: "SelfMod",
                deps: &["SelfMod"],
            }),
            Arc::new(Dummy {
                name: "OtherMod",
                deps: &[],
            }),
        ];
        let ordered = order_by_dependencies(modules);
        let names: Vec<&str> = ordered.iter().map(|module| module.name()).collect();
        assert_eq!(names, vec!["OtherMod", "SelfMod"]);
    }
}
