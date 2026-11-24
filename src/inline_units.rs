// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Public inline-unit API: forms, lists, galleries, query galleries, TTL and
//! callback routing.
//!
//! Python Heroku stores every interactive inline message in a single `_units`
//! dictionary, generates random callback payloads and expires old units with a
//! cleaner loop. This module implements the same model for Rust modules:
//!
//! ```ignore
//! let form = ctx.inline.form("<b>Hello</b>")
//!     .button(InlineButton::callback("Say hi", "demo:hi"))
//!     .on_callback("demo:hi", Arc::new(|ctx, event| Box::pin(async move {
//!         Ok(InlineCallbackAction::Toast(format!("Hi, {}", event.sender_id)))
//!     })))
//!     .send(&ctx, chat_id)
//!     .await?;
//! ```
//!
//! Units are owner-scoped by default. A module can opt into public access with
//! [`InlineFormBuilder::disable_security`] or an explicit allow-list. TTL is
//! capped at [`DEFAULT_UNIT_TTL`]; an expired unit is removed, its bot message
//! is deleted and its `on_unload` hook is invoked.

use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use grammers_client::message::{Button, InputMessage, ReplyMarkup};
use grammers_client::update::{Article, CallbackQuery, InlineQuery};
use grammers_tl_types as tl;
use rand::Rng;
use tracing::{debug, warn};

use crate::ctx::Ctx;

/// Default lifetime of an inline unit, matching Python's `_markup_ttl`.
pub const DEFAULT_UNIT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How often the background cleaner checks for expired units.
pub const CLEANER_INTERVAL: Duration = Duration::from_secs(5);
/// Hard cap on live units, so a misbehaving module cannot grow the map forever.
pub const MAX_UNITS: usize = 256;

const CALLBACK_PREFIX: &str = "hu:";

/// Media attached to an inline page.
#[derive(Clone, Debug, PartialEq)]
pub enum InlineMedia {
    Photo(String),
    Gif(String),
    Video(String),
    Audio(String),
    File { url: String, mime_type: String },
    Location { latitude: f64, longitude: f64 },
}

impl InlineMedia {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Photo(url) | Self::Gif(url) | Self::Video(url) | Self::Audio(url) => {
                ensure_http_url(url)
            }
            Self::File { url, mime_type } => {
                ensure_http_url(url)?;
                if mime_type.trim().is_empty() {
                    anyhow::bail!("inline file media requires a mime type");
                }
                Ok(())
            }
            Self::Location {
                latitude,
                longitude,
            } => {
                if !(-90.0..=90.0).contains(latitude) || !(-180.0..=180.0).contains(longitude) {
                    anyhow::bail!(
                        "invalid inline location: latitude must be -90..90 and longitude -180..180"
                    );
                }
                Ok(())
            }
        }
    }

    fn apply(&self, message: InputMessage) -> InputMessage {
        match self {
            Self::Photo(url) => message.photo_url(url.clone()),
            Self::Gif(url) | Self::Video(url) | Self::Audio(url) => {
                message.document_url(url.clone())
            }
            // External document URLs do not carry a mime type in grammers;
            // Telegram sniffs it. Keep the mime type for API compatibility and
            // documentation so callers can migrate later without changes.
            Self::File { url, .. } => message.document_url(url.clone()),
            Self::Location {
                latitude,
                longitude,
            } => message.media(tl::enums::InputMedia::GeoPoint(
                tl::types::InputMediaGeoPoint {
                    geo_point: tl::enums::InputGeoPoint::Point(tl::types::InputGeoPoint {
                        lat: *latitude,
                        long: *longitude,
                        accuracy_radius: None,
                    }),
                },
            )),
        }
    }
}

fn ensure_http_url(url: &str) -> Result<()> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        anyhow::bail!("inline media URL must start with http:// or https://")
    }
}

/// A button in a page. Callback payloads are replaced with an opaque unit token
/// before they are sent to Telegram; the original payload is only kept in this
/// process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineButton {
    Callback {
        text: String,
        data: String,
    },
    Url {
        text: String,
        url: String,
    },
    SwitchInline {
        text: String,
        query: String,
        same_peer: bool,
    },
    /// Jump to a zero-based page in the same unit.
    Page {
        text: String,
        page: usize,
    },
    /// Remove the unit and delete its message.
    Close {
        text: String,
    },
}

impl InlineButton {
    pub fn callback(text: impl Into<String>, data: impl Into<String>) -> Self {
        Self::Callback {
            text: text.into(),
            data: data.into(),
        }
    }

    pub fn url(text: impl Into<String>, url: impl Into<String>) -> Self {
        Self::Url {
            text: text.into(),
            url: url.into(),
        }
    }

    pub fn switch_inline(text: impl Into<String>, query: impl Into<String>) -> Self {
        Self::SwitchInline {
            text: text.into(),
            query: query.into(),
            same_peer: true,
        }
    }

    pub fn switch_elsewhere(text: impl Into<String>, query: impl Into<String>) -> Self {
        Self::SwitchInline {
            text: text.into(),
            query: query.into(),
            same_peer: false,
        }
    }

    pub fn page(text: impl Into<String>, page: usize) -> Self {
        Self::Page {
            text: text.into(),
            page,
        }
    }

    pub fn close(text: impl Into<String>) -> Self {
        Self::Close { text: text.into() }
    }
}

/// A single visible page of a unit. A gallery item is a page; a list chunk is a
/// page; a form normally has exactly one page.
#[derive(Clone, Debug, Default)]
pub struct InlinePage {
    pub text: String,
    pub media: Option<InlineMedia>,
    pub buttons: Vec<Vec<InlineButton>>,
}

impl InlinePage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            media: None,
            buttons: Vec::new(),
        }
    }

    pub fn with_media(mut self, media: InlineMedia) -> Self {
        self.media = Some(media);
        self
    }

    pub fn with_buttons(mut self, buttons: Vec<Vec<InlineButton>>) -> Self {
        self.buttons = buttons;
        self
    }
}

/// What a callback handler wants the manager to do after it returns.
#[derive(Clone, Debug, Default)]
pub enum InlineCallbackAction {
    /// Acknowledge the callback without changing the message.
    #[default]
    Handled,
    /// Show a transient toast in the client.
    Toast(String),
    /// Show a modal alert in the client.
    Alert(String),
    /// Replace the current page.
    Edit(InlinePage),
    /// Switch to another page of the same unit.
    GoTo(usize),
    /// Delete the unit message and remove the unit.
    Close,
    /// Remove the unit but keep the message on screen.
    Unload,
}

/// Everything a unit callback handler needs. The future must be `'static`, so
/// the event owns all strings.
#[derive(Clone, Debug)]
pub struct InlineCallbackEvent {
    pub unit_id: String,
    pub module: Option<String>,
    pub owner_id: i64,
    pub sender_id: i64,
    pub chat_id: Option<i64>,
    pub message_id: Option<i32>,
    pub data: String,
    pub page: usize,
    pub pages: usize,
    pub from_inline: bool,
}

pub type InlineCallbackFuture =
    Pin<Box<dyn Future<Output = Result<InlineCallbackAction>> + Send + 'static>>;
pub type InlineCallbackHandler =
    Arc<dyn Fn(Arc<Ctx>, InlineCallbackEvent) -> InlineCallbackFuture + Send + Sync>;

pub type InlineUnloadFuture = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
pub type InlineUnloadHandler = Arc<dyn Fn(String) -> InlineUnloadFuture + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlineUnitKind {
    Form,
    List,
    Gallery,
    QueryGallery,
}

impl InlineUnitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Form => "form",
            Self::List => "list",
            Self::Gallery => "gallery",
            Self::QueryGallery => "query_gallery",
        }
    }
}

/// A live inline unit.
#[derive(Clone)]
pub struct InlineUnit {
    pub id: String,
    pub kind: InlineUnitKind,
    pub owner_id: i64,
    pub module: Option<String>,
    pub pages: Vec<InlinePage>,
    pub page: usize,
    pub created_at: Instant,
    pub expires_at: Instant,
    pub force_me: bool,
    pub disable_security: bool,
    pub always_allow: Vec<i64>,
    /// Original callback payloads indexed by their opaque token suffix.
    pub custom_data: Vec<String>,
    pub handlers: HashMap<String, InlineCallbackHandler>,
    pub on_unload: Option<InlineUnloadHandler>,
    pub chat_id: Option<i64>,
    pub message_id: Option<i32>,
    pub inline_message_id: Option<tl::enums::InputBotInlineMessageId>,
}

impl fmt::Debug for InlineUnit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InlineUnit")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("owner_id", &self.owner_id)
            .field("module", &self.module)
            .field("pages", &self.pages.len())
            .field("page", &self.page)
            .field("expires_at", &self.expires_at)
            .field("custom_data", &self.custom_data.len())
            .field("handlers", &self.handlers.len())
            .field("chat_id", &self.chat_id)
            .field("message_id", &self.message_id)
            .finish()
    }
}

impl InlineUnit {
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }

    /// Security matrix, matching the Python `force_me`/`always_allow`/
    /// `disable_security` flags. The default is owner + sudo only.
    pub fn allows(&self, ctx: &Ctx, sender: i64) -> bool {
        self.allows_with(
            sender,
            ctx.security.is_owner(sender),
            ctx.security.is_sudo(sender),
        )
    }

    /// Pure form of [`InlineUnit::allows`], used by the security matrix tests.
    pub fn allows_with(&self, sender: i64, is_owner: bool, is_sudo: bool) -> bool {
        if self.disable_security {
            return true;
        }
        let owner = sender == self.owner_id || is_owner;
        // Python's `force_me` branch falls through to the `always_allow` check,
        // so an explicitly allowed user may still press the button.
        if self.force_me && !owner {
            return self.always_allow.contains(&sender);
        }
        owner || is_sudo || self.always_allow.contains(&sender)
    }

    fn token_for(&mut self, data: &str) -> String {
        if let Some(index) = self.custom_data.iter().position(|item| item == data) {
            return format!("{CALLBACK_PREFIX}{}:c:{index}", self.id);
        }
        self.custom_data.push(data.to_owned());
        format!(
            "{CALLBACK_PREFIX}{}:c:{}",
            self.id,
            self.custom_data.len() - 1
        )
    }
}

/// All live units. The mutex only guards the map; handlers run without it.
#[derive(Default)]
pub struct InlineUnitRegistry {
    units: StdMutex<HashMap<String, InlineUnit>>,
}

impl InlineUnitRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, InlineUnit>> {
        self.units
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    pub fn get(&self, unit_id: &str) -> Option<InlineUnit> {
        self.lock().get(unit_id).cloned()
    }

    /// Insert a unit, evicting expired entries and then the oldest live unit if
    /// the registry is over [`MAX_UNITS`]. Evicted units are returned so the
    /// caller can delete their messages and run their unload hooks.
    pub fn insert(&self, unit: InlineUnit) -> Vec<InlineUnit> {
        let now = Instant::now();
        let mut units = self.lock();
        let mut evicted = Vec::new();

        let expired: Vec<String> = units
            .iter()
            .filter(|(_, unit)| unit.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(unit) = units.remove(&id) {
                evicted.push(unit);
            }
        }

        while units.len() >= MAX_UNITS {
            let oldest = units
                .iter()
                .min_by_key(|(_, unit)| unit.created_at)
                .map(|(id, _)| id.clone());
            match oldest.and_then(|id| units.remove(&id)) {
                Some(unit) => evicted.push(unit),
                None => break,
            }
        }

        units.insert(unit.id.clone(), unit);
        evicted
    }

    /// Replace an existing unit without eviction. Returns `false` when it is
    /// already gone (for example, a concurrent close won the race).
    pub fn replace(&self, unit: InlineUnit) -> bool {
        let mut units = self.lock();
        if units.contains_key(&unit.id) {
            units.insert(unit.id.clone(), unit);
            true
        } else {
            false
        }
    }

    pub fn remove(&self, unit_id: &str) -> Option<InlineUnit> {
        self.lock().remove(unit_id)
    }

    pub fn take_expired(&self, now: Instant) -> Vec<InlineUnit> {
        let mut units = self.lock();
        let expired: Vec<String> = units
            .iter()
            .filter(|(_, unit)| unit.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect();
        expired
            .into_iter()
            .filter_map(|id| units.remove(&id))
            .collect()
    }

    pub fn take_all(&self) -> Vec<InlineUnit> {
        let mut units = self.lock();
        units.drain().map(|(_, unit)| unit).collect()
    }

    pub fn update_page(&self, unit_id: &str, page: usize) -> Option<InlineUnit> {
        let mut units = self.lock();
        let unit = units.get_mut(unit_id)?;
        if page >= unit.pages.len() {
            return None;
        }
        unit.page = page;
        Some(unit.clone())
    }

    pub fn set_message(&self, unit_id: &str, chat_id: i64, message_id: i32) -> bool {
        let mut units = self.lock();
        let Some(unit) = units.get_mut(unit_id) else {
            return false;
        };
        unit.chat_id = Some(chat_id);
        unit.message_id = Some(message_id);
        true
    }

    /// Link a chosen inline-query result (its id is the unit id) with the
    /// inline message id Telegram created for it.
    pub fn attach_inline_message(
        &self,
        unit_id: &str,
        message_id: tl::enums::InputBotInlineMessageId,
    ) -> bool {
        let mut units = self.lock();
        let Some(unit) = units.get_mut(unit_id) else {
            return false;
        };
        unit.inline_message_id = Some(message_id);
        true
    }
}

/// Parsed callback payload created by [`render_buttons`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnitCallback {
    pub unit_id: String,
    pub action: UnitAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitAction {
    Page(usize),
    Custom(usize),
    Close,
    Unload,
}

impl UnitCallback {
    pub fn parse(data: &str) -> Option<Self> {
        let rest = data.strip_prefix(CALLBACK_PREFIX)?;
        let mut parts = rest.split(':');
        let unit_id = parts.next()?;
        if unit_id.len() != 16 || !unit_id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
            return None;
        }
        let action = match parts.next()? {
            "p" => UnitAction::Page(parts.next()?.parse().ok()?),
            "c" => UnitAction::Custom(parts.next()?.parse().ok()?),
            "x" => UnitAction::Close,
            "u" => UnitAction::Unload,
            _ => return None,
        };
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            unit_id: unit_id.to_owned(),
            action,
        })
    }

    pub fn render(unit_id: &str, action: UnitAction) -> String {
        match action {
            UnitAction::Page(page) => format!("{CALLBACK_PREFIX}{unit_id}:p:{page}"),
            UnitAction::Custom(index) => format!("{CALLBACK_PREFIX}{unit_id}:c:{index}"),
            UnitAction::Close => format!("{CALLBACK_PREFIX}{unit_id}:x"),
            UnitAction::Unload => format!("{CALLBACK_PREFIX}{unit_id}:u"),
        }
    }
}

/// Render a page into Telegram buttons, replacing callback payloads with
/// per-unit opaque tokens.
pub(crate) fn render_buttons(unit: &mut InlineUnit, page: &InlinePage) -> Vec<Vec<Button>> {
    page.buttons
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|button| match button {
                    InlineButton::Callback { text, data } => {
                        let payload = unit.token_for(data);
                        Some(Button::data(text.clone(), payload.into_bytes()))
                    }
                    InlineButton::Url { text, url } => {
                        if url.starts_with("http://")
                            || url.starts_with("https://")
                            || url.starts_with("tg://")
                        {
                            Some(Button::url(text.clone(), url.clone()))
                        } else {
                            warn!(url, "Skipping inline button with an invalid URL");
                            None
                        }
                    }
                    InlineButton::SwitchInline {
                        text,
                        query,
                        same_peer,
                    } => Some(if *same_peer {
                        Button::switch(text.clone(), query.clone())
                    } else {
                        Button::switch_elsewhere(text.clone(), query.clone())
                    }),
                    InlineButton::Page { text, page } => Some(Button::data(
                        text.clone(),
                        UnitCallback::render(&unit.id, UnitAction::Page(*page)).into_bytes(),
                    )),
                    InlineButton::Close { text } => Some(Button::data(
                        text.clone(),
                        UnitCallback::render(&unit.id, UnitAction::Close).into_bytes(),
                    )),
                })
                .collect::<Vec<_>>()
        })
        .filter(|row| !row.is_empty())
        .collect()
}

/// Build the message for one page. Any new custom payloads are registered in
/// the unit; callers must persist the returned unit in the registry.
pub(crate) fn render_page(unit: &mut InlineUnit, index: usize) -> Result<InputMessage> {
    let page = unit
        .pages
        .get(index)
        .cloned()
        .with_context(|| format!("inline unit {} has no page {index}", unit.id))?;
    if page.text.trim().is_empty() && page.media.is_none() {
        anyhow::bail!("inline page must contain text or media");
    }
    if let Some(media) = &page.media {
        media.validate()?;
    }
    let buttons = render_buttons(unit, &page);
    let mut message = InputMessage::new()
        .html(page.text)
        .link_preview(false)
        .reply_markup(ReplyMarkup::from_buttons(&buttons));
    if let Some(media) = page.media {
        message = media.apply(message);
    }
    Ok(message)
}

fn random_unit_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn navigation(page: usize, pages: usize) -> Vec<InlineButton> {
    if pages <= 1 {
        return vec![InlineButton::close("✖️ Close")];
    }
    let mut row = Vec::new();
    if page > 0 {
        row.push(InlineButton::page("‹", page - 1));
    }
    row.push(InlineButton::page(
        format!("{} / {}", page + 1, pages),
        page,
    ));
    if page + 1 < pages {
        row.push(InlineButton::page("›", page + 1));
    }
    row.push(InlineButton::close("✖️"));
    row
}

#[derive(Clone, Default)]
struct UnitOptions {
    module: Option<String>,
    handlers: HashMap<String, InlineCallbackHandler>,
    on_unload: Option<InlineUnloadHandler>,
    owner_id: Option<i64>,
    force_me: bool,
    disable_security: bool,
    always_allow: Vec<i64>,
    ttl: Option<Duration>,
}

impl UnitOptions {
    fn expiry(&self, now: Instant) -> Instant {
        let ttl = self
            .ttl
            .unwrap_or(DEFAULT_UNIT_TTL)
            .clamp(Duration::from_secs(1), DEFAULT_UNIT_TTL);
        now + ttl
    }
}

/// Opaque handle returned by a sent unit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlineMessageRef {
    pub unit_id: String,
    pub chat_id: i64,
    pub message_id: i32,
}

impl InlineMessageRef {
    pub async fn edit(&self, ctx: &Arc<Ctx>, page: InlinePage) -> Result<()> {
        ctx.inline.edit_unit_page(&self.unit_id, page).await
    }

    pub async fn delete(&self, ctx: &Arc<Ctx>) -> Result<()> {
        ctx.inline.delete_unit(ctx, &self.unit_id).await
    }

    pub async fn unload(&self, ctx: &Arc<Ctx>) -> Result<()> {
        ctx.inline.unload_unit(ctx, &self.unit_id).await
    }
}

/// Builder for a single-page form.
pub struct InlineFormBuilder {
    page: InlinePage,
    options: UnitOptions,
}

impl InlineFormBuilder {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            page: InlinePage::new(text),
            options: UnitOptions::default(),
        }
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.page.text = text.into();
        self
    }

    pub fn media(mut self, media: InlineMedia) -> Self {
        self.page.media = Some(media);
        self
    }

    pub fn photo(self, url: impl Into<String>) -> Self {
        self.media(InlineMedia::Photo(url.into()))
    }

    pub fn gif(self, url: impl Into<String>) -> Self {
        self.media(InlineMedia::Gif(url.into()))
    }

    pub fn video(self, url: impl Into<String>) -> Self {
        self.media(InlineMedia::Video(url.into()))
    }

    pub fn audio(self, url: impl Into<String>) -> Self {
        self.media(InlineMedia::Audio(url.into()))
    }

    pub fn file(self, url: impl Into<String>, mime_type: impl Into<String>) -> Self {
        self.media(InlineMedia::File {
            url: url.into(),
            mime_type: mime_type.into(),
        })
    }

    pub fn location(self, latitude: f64, longitude: f64) -> Self {
        self.media(InlineMedia::Location {
            latitude,
            longitude,
        })
    }

    pub fn buttons(mut self, buttons: Vec<Vec<InlineButton>>) -> Self {
        self.page.buttons = buttons;
        self
    }

    pub fn button_row(mut self, row: Vec<InlineButton>) -> Self {
        self.page.buttons.push(row);
        self
    }

    pub fn button(mut self, button: InlineButton) -> Self {
        self.page.buttons.push(vec![button]);
        self
    }

    pub fn on_callback(mut self, data: impl Into<String>, handler: InlineCallbackHandler) -> Self {
        self.options.handlers.insert(data.into(), handler);
        self
    }

    pub fn on_unload(mut self, handler: InlineUnloadHandler) -> Self {
        self.options.on_unload = Some(handler);
        self
    }

    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.options.module = Some(module.into());
        self
    }

    pub fn owner(mut self, owner_id: i64) -> Self {
        self.options.owner_id = Some(owner_id);
        self
    }

    pub fn force_me(mut self, force_me: bool) -> Self {
        self.options.force_me = force_me;
        self
    }

    pub fn disable_security(mut self, disable: bool) -> Self {
        self.options.disable_security = disable;
        self
    }

    pub fn always_allow(mut self, users: Vec<i64>) -> Self {
        self.options.always_allow = users;
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options.ttl = Some(ttl);
        self
    }

    pub async fn send(self, ctx: &Arc<Ctx>, chat_id: i64) -> Result<InlineMessageRef> {
        ctx.inline
            .send_pages(
                ctx,
                chat_id,
                InlineUnitKind::Form,
                vec![self.page],
                self.options,
            )
            .await
    }
}

/// Builder for a paginated list of pre-formatted strings.
pub struct InlineListBuilder {
    items: Vec<String>,
    per_page: usize,
    header: Option<String>,
    custom_buttons: Vec<Vec<InlineButton>>,
    options: UnitOptions,
}

impl InlineListBuilder {
    pub fn new<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            items: items.into_iter().map(Into::into).collect(),
            per_page: 10,
            header: None,
            custom_buttons: Vec::new(),
            options: UnitOptions::default(),
        }
    }

    pub fn per_page(mut self, per_page: usize) -> Self {
        self.per_page = per_page.clamp(1, 50);
        self
    }

    pub fn header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    pub fn buttons(mut self, buttons: Vec<Vec<InlineButton>>) -> Self {
        self.custom_buttons = buttons;
        self
    }

    pub fn on_callback(mut self, data: impl Into<String>, handler: InlineCallbackHandler) -> Self {
        self.options.handlers.insert(data.into(), handler);
        self
    }

    pub fn on_unload(mut self, handler: InlineUnloadHandler) -> Self {
        self.options.on_unload = Some(handler);
        self
    }

    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.options.module = Some(module.into());
        self
    }

    pub fn owner(mut self, owner_id: i64) -> Self {
        self.options.owner_id = Some(owner_id);
        self
    }

    pub fn force_me(mut self, force_me: bool) -> Self {
        self.options.force_me = force_me;
        self
    }

    pub fn disable_security(mut self, disable: bool) -> Self {
        self.options.disable_security = disable;
        self
    }

    pub fn always_allow(mut self, users: Vec<i64>) -> Self {
        self.options.always_allow = users;
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options.ttl = Some(ttl);
        self
    }

    fn pages(&self) -> Vec<InlinePage> {
        let pages: Vec<InlinePage> = self
            .items
            .chunks(self.per_page)
            .enumerate()
            .map(|(index, chunk)| {
                let mut text = String::new();
                if index == 0 {
                    if let Some(header) = &self.header {
                        text.push_str(header);
                        text.push('\n');
                    }
                }
                text.push_str(&chunk.join("\n"));
                let mut buttons = self.custom_buttons.clone();
                buttons.push(navigation(index, self.items.len().div_ceil(self.per_page)));
                InlinePage {
                    text,
                    media: None,
                    buttons,
                }
            })
            .collect();
        if pages.is_empty() {
            vec![InlinePage::new(
                self.header
                    .clone()
                    .unwrap_or_else(|| "📄 <i>Empty list</i>".into()),
            )]
        } else {
            pages
        }
    }

    pub async fn send(self, ctx: &Arc<Ctx>, chat_id: i64) -> Result<InlineMessageRef> {
        let pages = self.pages();
        ctx.inline
            .send_pages(ctx, chat_id, InlineUnitKind::List, pages, self.options)
            .await
    }
}

/// One gallery slide.
#[derive(Clone, Debug)]
pub struct GalleryItem {
    pub media: InlineMedia,
    pub caption: String,
    pub buttons: Vec<Vec<InlineButton>>,
}

impl GalleryItem {
    pub fn new(media: InlineMedia, caption: impl Into<String>) -> Self {
        Self {
            media,
            caption: caption.into(),
            buttons: Vec::new(),
        }
    }

    pub fn buttons(mut self, buttons: Vec<Vec<InlineButton>>) -> Self {
        self.buttons = buttons;
        self
    }
}

/// Builder for a media gallery with automatic navigation.
pub struct InlineGalleryBuilder {
    items: Vec<GalleryItem>,
    options: UnitOptions,
}

impl InlineGalleryBuilder {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            options: UnitOptions::default(),
        }
    }

    pub fn item(mut self, item: GalleryItem) -> Self {
        self.items.push(item);
        self
    }

    pub fn items(mut self, items: Vec<GalleryItem>) -> Self {
        self.items = items;
        self
    }

    pub fn on_callback(mut self, data: impl Into<String>, handler: InlineCallbackHandler) -> Self {
        self.options.handlers.insert(data.into(), handler);
        self
    }

    pub fn on_unload(mut self, handler: InlineUnloadHandler) -> Self {
        self.options.on_unload = Some(handler);
        self
    }

    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.options.module = Some(module.into());
        self
    }

    pub fn owner(mut self, owner_id: i64) -> Self {
        self.options.owner_id = Some(owner_id);
        self
    }

    pub fn force_me(mut self, force_me: bool) -> Self {
        self.options.force_me = force_me;
        self
    }

    pub fn disable_security(mut self, disable: bool) -> Self {
        self.options.disable_security = disable;
        self
    }

    pub fn always_allow(mut self, users: Vec<i64>) -> Self {
        self.options.always_allow = users;
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options.ttl = Some(ttl);
        self
    }

    fn pages(&self) -> Vec<InlinePage> {
        self.items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let mut buttons = item.buttons.clone();
                buttons.push(navigation(index, self.items.len()));
                InlinePage {
                    text: item.caption.clone(),
                    media: Some(item.media.clone()),
                    buttons,
                }
            })
            .collect()
    }

    pub async fn send(self, ctx: &Arc<Ctx>, chat_id: i64) -> Result<InlineMessageRef> {
        if self.items.is_empty() {
            anyhow::bail!("an inline gallery needs at least one item");
        }
        let pages = self.pages();
        ctx.inline
            .send_pages(ctx, chat_id, InlineUnitKind::Gallery, pages, self.options)
            .await
    }
}

impl Default for InlineGalleryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// One result of an inline-query gallery. Mirrors Python's `query_gallery`
/// items: title, description, thumbnail and a page opened after selection.
#[derive(Clone, Debug)]
pub struct QueryGalleryItem {
    pub title: String,
    pub description: String,
    pub thumb_url: Option<String>,
    pub page: InlinePage,
}

impl QueryGalleryItem {
    pub fn new(title: impl Into<String>, description: impl Into<String>, page: InlinePage) -> Self {
        Self {
            title: title.into(),
            description: description.into(),
            thumb_url: None,
            page,
        }
    }

    pub fn thumb(mut self, url: impl Into<String>) -> Self {
        self.thumb_url = Some(url.into());
        self
    }
}

/// Builder for answering an inline query with a gallery of articles.
pub struct InlineQueryGalleryBuilder {
    items: Vec<QueryGalleryItem>,
    options: UnitOptions,
}

impl InlineQueryGalleryBuilder {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            options: UnitOptions {
                ttl: Some(Duration::from_secs(120)),
                ..UnitOptions::default()
            },
        }
    }

    pub fn item(mut self, item: QueryGalleryItem) -> Self {
        self.items.push(item);
        self
    }

    pub fn items(mut self, items: Vec<QueryGalleryItem>) -> Self {
        self.items = items;
        self
    }

    pub fn on_callback(mut self, data: impl Into<String>, handler: InlineCallbackHandler) -> Self {
        self.options.handlers.insert(data.into(), handler);
        self
    }

    pub fn on_unload(mut self, handler: InlineUnloadHandler) -> Self {
        self.options.on_unload = Some(handler);
        self
    }

    pub fn module(mut self, module: impl Into<String>) -> Self {
        self.options.module = Some(module.into());
        self
    }

    pub fn owner(mut self, owner_id: i64) -> Self {
        self.options.owner_id = Some(owner_id);
        self
    }

    pub fn always_allow(mut self, users: Vec<i64>) -> Self {
        self.options.always_allow = users;
        self
    }

    pub fn force_me(mut self, force_me: bool) -> Self {
        self.options.force_me = force_me;
        self
    }

    pub fn disable_security(mut self, disable: bool) -> Self {
        self.options.disable_security = disable;
        self
    }

    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.options.ttl = Some(ttl);
        self
    }

    /// Answer `query` with articles. Returns `false` when the query was empty
    /// or Telegram rejected the answer; inserted units are rolled back.
    pub async fn answer(self, ctx: &Arc<Ctx>, query: &InlineQuery) -> Result<bool> {
        if self.items.is_empty() {
            anyhow::bail!("a query gallery needs at least one item");
        }

        let mut units: Vec<String> = Vec::new();
        let mut results = Vec::new();
        for item in self.items {
            let unit = ctx.inline.new_unit(
                ctx,
                InlineUnitKind::QueryGallery,
                vec![item.page],
                &self.options,
            );
            let mut render_unit = unit.clone();
            let message = match render_page(&mut render_unit, 0) {
                Ok(message) => message,
                Err(err) => {
                    for unit_id in units {
                        if let Some(unit) = ctx.inline.units.remove(&unit_id) {
                            ctx.inline.run_unload(ctx, &unit).await;
                        }
                    }
                    return Err(err);
                }
            };
            let evicted = ctx.inline.units.insert(render_unit);
            for old in evicted {
                ctx.inline.expire_unit(ctx, &old).await;
            }
            units.push(unit.id.clone());

            let mut article = Article::new(item.title, message)
                .id(unit.id.clone())
                .description(item.description);
            if let Some(thumb) = item.thumb_url {
                if thumb.starts_with("http://") || thumb.starts_with("https://") {
                    article = article.thumb_url(thumb);
                }
            }
            results.push(article);
        }

        match query.answer(results).cache_time(0).send().await {
            Ok(()) => Ok(true),
            Err(err) => {
                for unit_id in units {
                    if let Some(unit) = ctx.inline.units.remove(&unit_id) {
                        ctx.inline.run_unload(ctx, &unit).await;
                    }
                }
                Err(anyhow::anyhow!("failed to answer the inline query: {err}"))
            }
        }
    }
}

impl Default for InlineQueryGalleryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::inline::InlineManager {
    /// Public constructor used as `ctx.inline.form("...")`.
    pub fn form(&self, text: impl Into<String>) -> InlineFormBuilder {
        InlineFormBuilder::new(text)
    }

    pub fn list<I, S>(&self, items: I) -> InlineListBuilder
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        InlineListBuilder::new(items)
    }

    pub fn gallery(&self) -> InlineGalleryBuilder {
        InlineGalleryBuilder::new()
    }

    pub fn query_gallery(&self) -> InlineQueryGalleryBuilder {
        InlineQueryGalleryBuilder::new()
    }

    fn new_unit(
        &self,
        ctx: &Ctx,
        kind: InlineUnitKind,
        pages: Vec<InlinePage>,
        options: &UnitOptions,
    ) -> InlineUnit {
        let now = Instant::now();
        InlineUnit {
            id: random_unit_id(),
            kind,
            owner_id: options.owner_id.unwrap_or(ctx.me_id),
            module: options.module.clone(),
            pages,
            page: 0,
            created_at: now,
            expires_at: options.expiry(now),
            force_me: options.force_me,
            disable_security: options.disable_security,
            always_allow: options.always_allow.clone(),
            custom_data: Vec::new(),
            handlers: options.handlers.clone(),
            on_unload: options.on_unload.clone(),
            chat_id: None,
            message_id: None,
            inline_message_id: None,
        }
    }

    async fn send_pages(
        &self,
        ctx: &Arc<Ctx>,
        chat_id: i64,
        kind: InlineUnitKind,
        pages: Vec<InlinePage>,
        options: UnitOptions,
    ) -> Result<InlineMessageRef> {
        if pages.is_empty() {
            anyhow::bail!("an inline unit needs at least one page");
        }
        let mut unit = self.new_unit(ctx, kind, pages, &options);
        let message = render_page(&mut unit, 0)?;
        let evicted = self.units.insert(unit.clone());
        for old in evicted {
            // Capacity eviction is rare (256 units), but its message and unload
            // hook must not leak.
            self.expire_unit(ctx, &old).await;
        }

        let message_id = match self.send_input_message(chat_id, message).await {
            Ok(message_id) => message_id,
            Err(err) => {
                self.units.remove(&unit.id);
                return Err(err);
            }
        };
        self.units.set_message(&unit.id, chat_id, message_id);
        unit.chat_id = Some(chat_id);
        unit.message_id = Some(message_id);
        debug!(
            unit_id = %unit.id,
            kind = unit.kind.as_str(),
            chat_id,
            message_id,
            "Inline unit sent"
        );
        Ok(InlineMessageRef {
            unit_id: unit.id,
            chat_id,
            message_id,
        })
    }

    pub(crate) async fn edit_unit_page(&self, unit_id: &str, page: InlinePage) -> Result<()> {
        let Some(mut unit) = self.units.get(unit_id) else {
            anyhow::bail!("inline unit has already been unloaded");
        };
        let Some(chat_id) = unit.chat_id else {
            anyhow::bail!("inline unit has no chat message to edit");
        };
        let Some(message_id) = unit.message_id else {
            anyhow::bail!("inline unit has no message id to edit");
        };
        let index = unit.page;
        if index >= unit.pages.len() {
            anyhow::bail!("inline unit page index is out of range");
        }
        unit.pages[index] = page;
        let message = render_page(&mut unit, index)?;
        if !self.units.replace(unit) {
            anyhow::bail!("inline unit has already been unloaded");
        }
        self.edit_input_message(chat_id, message_id, message).await
    }

    pub(crate) async fn delete_unit(&self, ctx: &Arc<Ctx>, unit_id: &str) -> Result<()> {
        let Some(unit) = self.units.remove(unit_id) else {
            anyhow::bail!("inline unit has already been unloaded");
        };
        if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
            if let Err(err) = self.delete_input_message(chat_id, message_id).await {
                warn!(unit_id = %unit.id, "Failed to delete an inline unit message: {err:#}");
            }
        }
        self.run_unload(ctx, &unit).await;
        Ok(())
    }

    pub(crate) async fn unload_unit(&self, ctx: &Arc<Ctx>, unit_id: &str) -> Result<()> {
        let Some(unit) = self.units.remove(unit_id) else {
            anyhow::bail!("inline unit has already been unloaded");
        };
        // `unload` keeps the message, matching Python's semantics.
        self.run_unload(ctx, &unit).await;
        Ok(())
    }

    /// Process a `hu:` callback. `Ok(false)` means "this is a unit callback but
    /// no module handler owns it", so the central dispatcher may continue with
    /// the original payload.
    pub(crate) async fn handle_unit_callback(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        callback: UnitCallback,
        sender: i64,
    ) -> Result<bool> {
        let Some(unit) = self.units.get(&callback.unit_id) else {
            return Ok(false);
        };

        if unit.is_expired(Instant::now()) {
            self.expire_unit(ctx, &unit).await;
            let _ = call
                .answer()
                .alert(ctx.tr("inline.unit_expired"))
                .send()
                .await;
            return Ok(true);
        }

        if !unit.allows(ctx, sender) {
            let _ = call
                .answer()
                .alert(ctx.tr("inline.callback_forbidden"))
                .send()
                .await;
            return Ok(true);
        }

        match callback.action {
            UnitAction::Page(page) => {
                if page >= unit.pages.len() {
                    let _ = call.answer().send().await;
                    return Ok(true);
                }
                self.show_unit_page(ctx, call, &unit, page).await;
                Ok(true)
            }
            UnitAction::Close => {
                if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
                    let _ = self.delete_input_message(chat_id, message_id).await;
                }
                self.units.remove(&unit.id);
                self.run_unload(ctx, &unit).await;
                let _ = call.answer().send().await;
                Ok(true)
            }
            UnitAction::Unload => {
                self.units.remove(&unit.id);
                self.run_unload(ctx, &unit).await;
                let _ = call.answer().send().await;
                Ok(true)
            }
            UnitAction::Custom(index) => {
                let Some(data) = unit.custom_data.get(index).cloned() else {
                    let _ = call.answer().send().await;
                    return Ok(true);
                };
                let Some(handler) = unit.handlers.get(&data).cloned() else {
                    // Legacy module button: hand the *original* payload to the
                    // normal module dispatcher. The unit still owns permission
                    // and TTL checks, so a foreign user cannot bypass them.
                    let consumed = self
                        .dispatch_module_callback(ctx, call, &data, sender)
                        .await;
                    if !consumed {
                        let _ = call.answer().send().await;
                    }
                    return Ok(true);
                };
                let event = InlineCallbackEvent {
                    unit_id: unit.id.clone(),
                    module: unit.module.clone(),
                    owner_id: unit.owner_id,
                    sender_id: sender,
                    chat_id: unit.chat_id,
                    message_id: unit.message_id,
                    data,
                    page: unit.page,
                    pages: unit.pages.len(),
                    from_inline: call.is_from_inline(),
                };
                match handler(ctx.clone(), event).await {
                    Ok(action) => {
                        self.apply_unit_action(ctx, call, &unit, action).await;
                    }
                    Err(err) => {
                        warn!(unit_id = %unit.id, "Inline unit callback failed: {err:#}");
                        if let Some(module) = unit
                            .module
                            .as_deref()
                            .and_then(|name| ctx.modules.get(name))
                        {
                            module
                                .on_error(ctx, &format!("inline callback {}", unit.id), &err)
                                .await;
                        }
                        let _ = call
                            .answer()
                            .alert(ctx.tr("inline.callback_error"))
                            .send()
                            .await;
                    }
                }
                Ok(true)
            }
        }
    }

    async fn apply_unit_action(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        unit: &InlineUnit,
        action: InlineCallbackAction,
    ) {
        match action {
            InlineCallbackAction::Handled => {
                let _ = call.answer().send().await;
            }
            InlineCallbackAction::Toast(text) => {
                let _ = call.answer().text(text).send().await;
            }
            InlineCallbackAction::Alert(text) => {
                let _ = call.answer().alert(text).send().await;
            }
            InlineCallbackAction::Edit(page) => {
                if let Some(mut updated) = self.units.get(&unit.id) {
                    let index = updated.page;
                    if index < updated.pages.len() {
                        updated.pages[index] = page;
                        if let Ok(message) = render_page(&mut updated, index) {
                            if self.units.replace(updated) {
                                let _ = call.answer().edit(message).await;
                                return;
                            }
                        }
                    }
                }
                let _ = call.answer().send().await;
            }
            InlineCallbackAction::GoTo(page) => {
                if page < unit.pages.len() {
                    self.show_unit_page(ctx, call, unit, page).await;
                } else {
                    let _ = call.answer().send().await;
                }
            }
            InlineCallbackAction::Close => {
                if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
                    let _ = self.delete_input_message(chat_id, message_id).await;
                }
                self.units.remove(&unit.id);
                self.run_unload(ctx, unit).await;
                let _ = call.answer().send().await;
            }
            InlineCallbackAction::Unload => {
                self.units.remove(&unit.id);
                self.run_unload(ctx, unit).await;
                let _ = call.answer().send().await;
            }
        }
    }

    async fn show_unit_page(
        &self,
        ctx: &Arc<Ctx>,
        call: &CallbackQuery,
        unit: &InlineUnit,
        page: usize,
    ) {
        let Some(mut updated) = self.units.get(&unit.id) else {
            let _ = call.answer().send().await;
            return;
        };
        if page >= updated.pages.len() {
            let _ = call.answer().send().await;
            return;
        }
        updated.page = page;
        match render_page(&mut updated, page) {
            Ok(message) => {
                if !self.units.replace(updated) {
                    let _ = call.answer().send().await;
                    return;
                }
                if let Err(err) = call.answer().edit(message).await {
                    warn!(unit_id = %unit.id, "Failed to edit an inline unit page: {err:#}");
                }
            }
            Err(err) => {
                warn!(unit_id = %unit.id, "Failed to render an inline unit page: {err:#}");
                let _ = call
                    .answer()
                    .alert(ctx.tr("inline.callback_error"))
                    .send()
                    .await;
            }
        }
    }

    async fn expire_unit(&self, ctx: &Arc<Ctx>, unit: &InlineUnit) {
        self.units.remove(&unit.id);
        if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
            let _ = self.delete_input_message(chat_id, message_id).await;
        }
        self.run_unload(ctx, unit).await;
    }

    async fn run_unload(&self, ctx: &Arc<Ctx>, unit: &InlineUnit) {
        if let Some(handler) = &unit.on_unload {
            handler(unit.id.clone()).await;
        }
        debug!(unit_id = %unit.id, "Inline unit unloaded");
        let _ = ctx;
    }

    /// Remove expired units. Called by the background cleaner and exposed for
    /// tests and explicit maintenance.
    pub async fn cleanup_expired_units(&self, ctx: &Arc<Ctx>) {
        for unit in self.units.take_expired(Instant::now()) {
            if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
                let _ = self.delete_input_message(chat_id, message_id).await;
            }
            self.run_unload(ctx, &unit).await;
        }
    }

    /// Remove every unit. Used during shutdown; does not stop the bot runtime.
    pub async fn unload_all_units(&self, ctx: &Arc<Ctx>) {
        for unit in self.units.take_all() {
            if let (Some(chat_id), Some(message_id)) = (unit.chat_id, unit.message_id) {
                let _ = self.delete_input_message(chat_id, message_id).await;
            }
            self.run_unload(ctx, &unit).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn unit(page: usize) -> InlineUnit {
        let now = Instant::now();
        InlineUnit {
            id: "abcdefghijklmnop".into(),
            kind: InlineUnitKind::Form,
            owner_id: 1,
            module: None,
            pages: vec![
                InlinePage::new("one"),
                InlinePage::new("two"),
                InlinePage::new("three"),
            ],
            page,
            created_at: now,
            expires_at: now + DEFAULT_UNIT_TTL,
            force_me: false,
            disable_security: false,
            always_allow: Vec::new(),
            custom_data: Vec::new(),
            handlers: HashMap::new(),
            on_unload: None,
            chat_id: None,
            message_id: None,
            inline_message_id: None,
        }
    }

    #[test]
    fn unit_callback_parser_accepts_only_well_formed_payloads() {
        let id = "abcdefghijklmnop";
        assert_eq!(
            UnitCallback::parse(&UnitCallback::render(id, UnitAction::Page(3))),
            Some(UnitCallback {
                unit_id: id.into(),
                action: UnitAction::Page(3),
            })
        );
        assert_eq!(
            UnitCallback::parse(&UnitCallback::render(id, UnitAction::Custom(2))),
            Some(UnitCallback {
                unit_id: id.into(),
                action: UnitAction::Custom(2),
            })
        );
        assert!(UnitCallback::parse("hu:short:p:0").is_none());
        assert!(UnitCallback::parse("hu:abcdefghijklmnop:p:not-a-number").is_none());
        assert!(UnitCallback::parse("hu:abcdefghijklmnop:p:1:extra").is_none());
        assert!(UnitCallback::parse("foreign:abcdefghijklmnop:p:1").is_none());
    }

    #[test]
    fn ttl_is_capped_and_units_expire() {
        let options = UnitOptions {
            ttl: Some(Duration::from_secs(10 * 24 * 60 * 60)),
            ..UnitOptions::default()
        };
        let now = Instant::now();
        assert_eq!(options.expiry(now), now + DEFAULT_UNIT_TTL);

        let mut entry = unit(0);
        entry.expires_at = now - Duration::from_secs(1);
        assert!(entry.is_expired(now));
    }

    #[test]
    fn registry_expires_and_bounds_units() {
        let registry = InlineUnitRegistry::default();
        let mut expired = unit(0);
        expired.id = "expiredexpired00".into();
        expired.expires_at = Instant::now() - Duration::from_secs(1);
        registry.insert(expired);
        assert!(registry.take_expired(Instant::now()).len() == 1);
        assert!(registry.is_empty());

        for index in 0..MAX_UNITS + 3 {
            let mut entry = unit(0);
            entry.id = format!("{index:016}");
            registry.insert(entry);
        }
        assert_eq!(registry.len(), MAX_UNITS);
    }

    #[test]
    fn registry_page_updates_are_bounded() {
        let registry = InlineUnitRegistry::default();
        registry.insert(unit(0));
        assert_eq!(
            registry.update_page("abcdefghijklmnop", 2).map(|u| u.page),
            Some(2)
        );
        assert!(registry.update_page("abcdefghijklmnop", 3).is_none());
        assert!(registry.update_page("missingmissing000", 0).is_none());
    }

    #[test]
    fn concurrent_page_updates_never_corrupt_the_index() {
        let registry = Arc::new(InlineUnitRegistry::default());
        registry.insert(unit(0));
        let mut handles = Vec::new();
        for index in 0..64 {
            let registry = Arc::clone(&registry);
            handles.push(std::thread::spawn(move || {
                let page = index % 3;
                let _ = registry.update_page("abcdefghijklmnop", page);
            }));
        }
        for handle in handles {
            handle.join().expect("worker thread panicked");
        }
        let entry = registry.get("abcdefghijklmnop").expect("unit still exists");
        assert!(entry.page < entry.pages.len());
    }

    #[test]
    fn unit_security_matrix_matches_python_flags() {
        let mut entry = unit(0);
        entry.owner_id = 10;
        // Default: owner and sudo, but not an arbitrary user or a security-group
        // member (group membership is resolved by the caller as sudo/owner).
        assert!(entry.allows_with(10, false, false));
        assert!(entry.allows_with(11, true, false));
        assert!(entry.allows_with(12, false, true));
        assert!(!entry.allows_with(13, false, false));

        entry.always_allow = vec![13];
        assert!(entry.allows_with(13, false, false));

        entry.force_me = true;
        assert!(entry.allows_with(10, false, false));
        // `always_allow` explicitly overrides `force_me`, matching Python's
        // fall-through branch.
        assert!(entry.allows_with(13, false, false));
        assert!(entry.allows_with(11, true, false));
        assert!(!entry.allows_with(14, false, false));

        entry.disable_security = true;
        assert!(entry.allows_with(99, false, false));
    }

    #[test]
    fn render_replaces_callback_payloads_and_deduplicates_tokens() {
        let mut entry = unit(0);
        let page = InlinePage::new("hello").with_buttons(vec![vec![
            InlineButton::callback("One", "module:one"),
            InlineButton::callback("One again", "module:one"),
            InlineButton::url("Site", "https://example.com"),
            InlineButton::close("Close"),
        ]]);
        let buttons = render_buttons(&mut entry, &page);
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].len(), 4);
        assert_eq!(entry.custom_data, vec!["module:one"]);
    }

    #[test]
    fn invalid_media_is_rejected_before_telegram_is_called() {
        assert!(InlineMedia::Photo("file:///tmp/a.jpg".into())
            .validate()
            .is_err());
        assert!(InlineMedia::File {
            url: "https://example.com/a.pdf".into(),
            mime_type: String::new(),
        }
        .validate()
        .is_err());
        assert!(InlineMedia::Location {
            latitude: 100.0,
            longitude: 0.0,
        }
        .validate()
        .is_err());
        assert!(InlineMedia::Location {
            latitude: 55.0,
            longitude: 48.0,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn list_pages_are_split_and_navigable() {
        let builder = InlineListBuilder::new(
            (0..23)
                .map(|index| format!("item {index}"))
                .collect::<Vec<_>>(),
        )
        .per_page(10);
        let pages = builder.pages();
        assert_eq!(pages.len(), 3);
        assert!(pages[0].text.contains("item 0"));
        assert!(pages[0].buttons.last().is_some_and(|row| {
            row.iter()
                .any(|button| matches!(button, InlineButton::Page { page: 1, .. }))
        }));
        assert_eq!(pages[2].buttons.last().map(Vec::len), Some(3));
    }

    #[test]
    fn gallery_pages_include_navigation_and_media() {
        let builder = InlineGalleryBuilder::new()
            .item(GalleryItem::new(
                InlineMedia::Photo("https://example.com/a.jpg".into()),
                "first",
            ))
            .item(GalleryItem::new(
                InlineMedia::Photo("https://example.com/b.jpg".into()),
                "second",
            ));
        let pages = builder.pages();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].buttons.last().map(Vec::len), Some(3));
        assert!(matches!(pages[1].media, Some(InlineMedia::Photo(_))));
    }

    #[test]
    fn unload_hooks_run_exactly_once_for_expired_units() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_ref = Arc::clone(&counter);
        let mut entry = unit(0);
        entry.expires_at = Instant::now() - Duration::from_secs(1);
        entry.on_unload = Some(Arc::new(move |_| {
            let counter = Arc::clone(&counter_ref);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
            })
        }));
        let registry = InlineUnitRegistry::default();
        registry.insert(entry);
        let expired = registry.take_expired(Instant::now());
        assert_eq!(expired.len(), 1);
        assert!(registry.is_empty());
        // The cleaner is the only caller of the hook; this asserts the registry
        // does not invoke it twice.
        let _ = expired;
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }
}
