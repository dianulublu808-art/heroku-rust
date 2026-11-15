// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Local Telegram API flood protection, compatible with Heroku's `APILimiter`.
//!
//! Every outgoing message action passes through [`guard`]. The guard counts
//! actions in a sliding window and, when the threshold is exceeded, freezes
//! outgoing API calls for a configurable amount of time. Owners can suspend the
//! protection with `.suspend_api_protect <seconds>`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::warn;

const DEFAULT_SAMPLE_SECS: u64 = 15;
const DEFAULT_THRESHOLD: usize = 100;
const DEFAULT_FREEZE_SECS: u64 = 30;

struct State {
    calls: VecDeque<Instant>,
    frozen_until: Option<Instant>,
    suspended_until: Option<Instant>,
    warning_pending: bool,
}

pub struct Protection {
    enabled: AtomicBool,
    threshold: AtomicUsize,
    sample_ms: AtomicU64,
    freeze_ms: AtomicU64,
    state: Mutex<State>,
}

impl Default for Protection {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            threshold: AtomicUsize::new(DEFAULT_THRESHOLD),
            sample_ms: AtomicU64::new(DEFAULT_SAMPLE_SECS * 1000),
            freeze_ms: AtomicU64::new(DEFAULT_FREEZE_SECS * 1000),
            state: Mutex::new(State {
                calls: VecDeque::new(),
                frozen_until: None,
                suspended_until: None,
                warning_pending: false,
            }),
        }
    }
}

static PROTECTION: OnceLock<Protection> = OnceLock::new();

pub fn protection() -> &'static Protection {
    PROTECTION.get_or_init(Protection::default)
}

impl Protection {
    pub fn configure(&self, enabled: bool, threshold: usize, sample_secs: u64, freeze_secs: u64) {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.threshold.store(threshold.max(1), Ordering::Relaxed);
        self.sample_ms
            .store(sample_secs.max(1) * 1000, Ordering::Relaxed);
        self.freeze_ms
            .store(freeze_secs.max(1) * 1000, Ordering::Relaxed);
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn threshold(&self) -> usize {
        self.threshold.load(Ordering::Relaxed)
    }

    pub fn sample_secs(&self) -> u64 {
        self.sample_ms.load(Ordering::Relaxed) / 1000
    }

    pub fn freeze_secs(&self) -> u64 {
        self.freeze_ms.load(Ordering::Relaxed) / 1000
    }

    /// Disable protection for the given amount of seconds.
    pub fn suspend(&self, seconds: u64) {
        if let Ok(mut state) = self.state.lock() {
            state.suspended_until = Some(Instant::now() + Duration::from_secs(seconds));
        }
    }

    pub fn suspended_for(&self) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        let now = Instant::now();
        match state.suspended_until {
            Some(until) if until > now => Some((until - now).as_secs()),
            Some(_) => {
                state.suspended_until = None;
                None
            }
            None => None,
        }
    }

    pub fn frozen_for(&self) -> Option<u64> {
        let mut state = self.state.lock().ok()?;
        let now = Instant::now();
        match state.frozen_until {
            Some(until) if until > now => Some((until - now).as_secs()),
            Some(_) => {
                state.frozen_until = None;
                None
            }
            None => None,
        }
    }

    /// Take the pending warning flag. Used by the main loop to notify the owner
    /// without blocking the API guard itself.
    pub fn take_warning(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        std::mem::replace(&mut state.warning_pending, false)
    }

    pub fn pending_calls(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.calls.len())
            .unwrap_or(0)
    }
}

/// Called before every outgoing message action.
pub async fn guard() {
    let protection = protection();
    if !protection.enabled() || protection.suspended_for().is_some() {
        return;
    }

    let now = Instant::now();
    let sample = Duration::from_millis(protection.sample_ms.load(Ordering::Relaxed));
    let freeze = Duration::from_millis(protection.freeze_ms.load(Ordering::Relaxed));
    let threshold = protection.threshold.load(Ordering::Relaxed).max(1);

    let wait = {
        let Ok(mut state) = protection.state.lock() else {
            return;
        };

        if let Some(until) = state.frozen_until {
            if until > now {
                Some(until - now)
            } else {
                state.frozen_until = None;
                state.calls.clear();
                None
            }
        } else {
            state
                .calls
                .retain(|call| now.duration_since(*call) < sample);
            state.calls.push_back(now);

            if state.calls.len() > threshold {
                state.calls.clear();
                let until = now + freeze;
                state.frozen_until = Some(until);
                state.warning_pending = true;
                warn!(
                    "API flood protection triggered: freezing outgoing requests for {}s",
                    freeze.as_secs()
                );
                Some(freeze)
            } else {
                None
            }
        }
    };

    if let Some(wait) = wait {
        tokio::time::sleep(wait).await;
    }
}
