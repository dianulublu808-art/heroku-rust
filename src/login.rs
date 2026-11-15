// ©️ Heroku Userbot — Rust rewrite
// Licensed under the GNU AGPLv3. See LICENSE.

//! Interactive MTProto login.
//!
//! The high-level `Client::request_login_code` helper hides *where* the code was
//! sent (app, SMS, call, e-mail) and cannot resend it. This module talks to
//! `auth.sendCode` directly so the bot can tell the user what to expect, retry
//! invalid codes, request a new code without a restart and handle 2FA.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use grammers_client::client::PasswordToken;
use grammers_client::{Client, SignInError};
use grammers_session::storages::ErasedSession;
use grammers_session::types::{PeerInfo, UpdateState, UpdatesState};
use grammers_tl_types as tl;
use tracing::info;

use crate::config::{self, Config};

const MAX_CODE_ATTEMPTS: u8 = 3;
const MAX_PASSWORD_ATTEMPTS: u8 = 3;

/// Log in interactively, printing friendly delivery hints and retrying when the
/// code is wrong or expired.
pub async fn interactive_login(
    client: &Client,
    session: &Arc<ErasedSession>,
    config: &Config,
) -> Result<()> {
    let phone = resolve_phone(config)?;
    let lang = config.lang.as_str();
    println!(
        "\n{}\n",
        l(lang, "🔐 Signing in to Telegram", "🔐 Вход в Telegram")
    );
    let mut sent = send_code(client, &phone, config.api_id, &config.api_hash, lang).await?;
    print_delivery(&sent, &phone, lang);

    let mut env_code = std::env::var("TG_CODE")
        .ok()
        .map(|code| code.trim().to_owned())
        .filter(|code| !code.is_empty());
    let mut attempts = 0u8;

    loop {
        let input = match env_code.take() {
            Some(code) => {
                println!(
                    "{}",
                    l(
                        lang,
                        "Using the login code from TG_CODE.",
                        "Использую код из TG_CODE."
                    )
                );
                code
            }
            None => config::prompt(l(
                lang,
                "Enter the login code (or type 'resend' to request a new one): ",
                "Введи код из Telegram (или 'resend' для повторной отправки): ",
            ))?,
        };
        let input = input.trim();

        if input.eq_ignore_ascii_case("resend") {
            sent = resend_code(client, &phone, &sent.phone_code_hash, lang).await?;
            print_delivery(&sent, &phone, lang);
            continue;
        }
        if input.is_empty() {
            continue;
        }

        match sign_in(client, session, &phone, &sent.phone_code_hash, input, lang).await {
            Ok(()) => return Ok(()),
            Err(LoginFailure::PasswordRequired) => {
                complete_two_factor(client, lang).await?;
                return Ok(());
            }
            Err(LoginFailure::InvalidCode) => {
                attempts += 1;
                if attempts >= MAX_CODE_ATTEMPTS {
                    bail!("too many invalid login codes");
                }
                println!(
                    "{}",
                    l(
                        lang,
                        "Wrong code, try again.",
                        "Неверный код, попробуй ещё раз."
                    )
                );
            }
            Err(LoginFailure::Expired) => {
                println!(
                    "{}",
                    l(
                        lang,
                        "The code expired, requesting a new one…",
                        "Код истёк, запрашиваю новый…"
                    )
                );
                sent = resend_code(client, &phone, &sent.phone_code_hash, lang).await?;
                print_delivery(&sent, &phone, lang);
            }
            Err(LoginFailure::SignUpRequired) => bail!(
                "{}",
                l(
                    lang,
                    "this phone number has no Telegram account; sign up in the official app first",
                    "у этого номера нет аккаунта Telegram; сначала зарегистрируйся в официальном приложении"
                )
            ),
            Err(LoginFailure::Other(err)) => bail!("{err}"),
        }
    }
}

/// Sign in by scanning a QR code from an already authorised Telegram app.
///
/// Mirrors Python's `qr_login`: export a login token, render it as a QR code,
/// poll `auth.importLoginToken` and recreate the token when it expires. Works
/// without a phone number, which makes it handy on a headless Termux setup.
pub async fn qr_login(
    client: &Client,
    session: &Arc<ErasedSession>,
    config: &Config,
) -> Result<()> {
    let lang = config.lang.as_str();

    println!("\n{}\n", l(lang, "🔐 QR code login", "🔐 Вход по QR-коду"));
    println!(
        "{}",
        l(
            lang,
            "Open Telegram → Settings → Devices → Link Desktop Device, then scan the code below.",
            "Открой Telegram → Настройки → Устройства → Подключить устройство по QR-коду и отсканируй код ниже."
        )
    );

    loop {
        let exported = client
            .invoke(&tl::functions::auth::ExportLoginToken {
                api_id: config.api_id,
                api_hash: config.api_hash.clone(),
                except_ids: Vec::new(),
            })
            .await
            .map_err(|err| anyhow::anyhow!("{}", describe_login_error(&err, lang)))?;

        let token = match exported {
            tl::enums::auth::LoginToken::Token(token) => token,
            tl::enums::auth::LoginToken::MigrateTo(migrate) => {
                // The account lives on another datacenter. Point the session at
                // it and export a fresh token there, like official clients do.
                session
                    .set_home_dc_id(migrate.dc_id)
                    .await
                    .map_err(|err| anyhow::anyhow!("failed to switch datacenter: {err}"))?;
                continue;
            }
            tl::enums::auth::LoginToken::Success(success) => {
                return finish_qr_login(success, client, session, lang).await;
            }
        };

        let url = format!("tg://login?token={}", base64_url_nopad(&token.token));
        render_qr(&url);
        println!(
            "\n{}\n{}\n",
            l(
                lang,
                "Waiting for the QR code to be scanned…",
                "Жду, пока отсканируют QR-код…"
            ),
            l(
                lang,
                "The code refreshes automatically. Press Ctrl+C to cancel.",
                "Код обновляется автоматически. Нажми Ctrl+C, чтобы отменить."
            )
        );
        println!(
            "{} {url}\n",
            l(lang, "Login link (copyable):", "Ссылка для входа:")
        );

        let expires = i64::from(token.expires);
        loop {
            if unix_now() + 1 >= expires {
                println!(
                    "{}",
                    l(
                        lang,
                        "QR code expired, generating a new one…",
                        "QR-код истёк, генерирую новый…"
                    )
                );
                break;
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            match client
                .invoke(&tl::functions::auth::ImportLoginToken {
                    token: token.token.clone(),
                })
                .await
            {
                Ok(tl::enums::auth::LoginToken::Token(_)) => continue,
                Ok(tl::enums::auth::LoginToken::Success(success)) => {
                    return finish_qr_login(success, client, session, lang).await;
                }
                Ok(tl::enums::auth::LoginToken::MigrateTo(migrate)) => {
                    session
                        .set_home_dc_id(migrate.dc_id)
                        .await
                        .map_err(|err| anyhow::anyhow!("failed to switch datacenter: {err}"))?;
                    match client
                        .invoke(&tl::functions::auth::ImportLoginToken {
                            token: migrate.token,
                        })
                        .await
                    {
                        Ok(tl::enums::auth::LoginToken::Success(success)) => {
                            return finish_qr_login(success, client, session, lang).await;
                        }
                        Ok(tl::enums::auth::LoginToken::Token(_)) => {
                            break;
                        }
                        Ok(tl::enums::auth::LoginToken::MigrateTo(_)) => {
                            bail!("Telegram asked to migrate datacenters more than once")
                        }
                        Err(err) => {
                            bail!("{}", describe_login_error(&err, lang));
                        }
                    }
                }
                Err(err)
                    if err.is("AUTH_TOKEN_EXPIRED")
                        || err.is("AUTH_TOKEN_INVALID")
                        || err.is("AUTH_TOKEN_ALREADY_ACCEPTED") =>
                {
                    break;
                }
                Err(err) => bail!("{}", describe_login_error(&err, lang)),
            }
        }
    }
}

async fn finish_qr_login(
    success: tl::types::auth::LoginTokenSuccess,
    client: &Client,
    session: &Arc<ErasedSession>,
    lang: &str,
) -> Result<()> {
    match success.authorization {
        tl::enums::auth::Authorization::Authorization(_) => {
            finish_login(client, session).await?;
            info!(
                "{}",
                l(lang, "Signed in successfully", "Вход выполнен успешно")
            );
            Ok(())
        }
        tl::enums::auth::Authorization::SignUpRequired(_) => bail!(
            "{}",
            l(
                lang,
                "this account still needs to sign up in the official Telegram app",
                "этот аккаунт ещё должен зарегистрироваться в официальном приложении Telegram"
            )
        ),
    }
}

/// URL-safe base64 without padding, as required by `tg://login?token=…`.
fn base64_url_nopad(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 63] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(triple >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[triple as usize & 63] as char);
        }
    }
    out
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Print a compact Unicode QR code (two module rows per terminal line).
fn render_qr(url: &str) {
    use qrcodegen::{QrCode, QrCodeEcc};

    let qr = match QrCode::encode_text(url, QrCodeEcc::Medium) {
        Ok(qr) => qr,
        Err(err) => {
            println!("Could not build a QR code: {err}");
            return;
        }
    };

    let border = 2;
    let size = qr.size();
    let total = size + border * 2;
    let dark = |x: i32, y: i32| {
        x >= border
            && y >= border
            && x < size + border
            && y < size + border
            && qr.get_module(x - border, y - border)
    };

    // Clear the screen and move the cursor to the top-left corner.
    print!("\x1b[2J\x1b[3;1f");
    for y in (0..total).step_by(2) {
        let mut line = String::with_capacity(total as usize);
        for x in 0..total {
            let top = dark(x, y);
            let bottom = dark(x, y + 1);
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("{line}");
    }
}

/// Validate and canonicalise a phone number: `+<country><number>` with digits
/// only. Spaces, dashes, dots and parentheses are accepted and stripped.
pub fn normalize_phone(raw: &str) -> Result<String> {
    let compact: String = raw
        .trim()
        .chars()
        .filter(|ch| !matches!(ch, ' ' | '\t' | '-' | '(' | ')' | '.'))
        .collect();

    if compact.is_empty() {
        bail!("a phone number is required to log in");
    }

    let with_plus = if let Some(rest) = compact.strip_prefix("00") {
        format!("+{rest}")
    } else if compact.starts_with('+') {
        compact
    } else if compact.len() == 11 && compact.starts_with('8') {
        // Common Russian input style: 8XXXXXXXXXX → +7XXXXXXXXXX.
        format!("+7{}", &compact[1..])
    } else {
        bail!("phone number must start with '+' and the country code, e.g. +79001234567");
    };

    let digits = &with_plus[1..];
    if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
        bail!("phone number must contain only digits, spaces, dashes and parentheses");
    }
    if !(7..=15).contains(&digits.len()) {
        bail!(
            "phone number must contain 7 to 15 digits after '+', got {}",
            digits.len()
        );
    }

    Ok(with_plus)
}

fn resolve_phone(config: &Config) -> Result<String> {
    let raw = match config.phone.clone() {
        Some(phone) if !phone.trim().is_empty() => phone,
        _ => {
            let phone = config::prompt("Enter your phone number in international format (+...): ")?;
            if phone.is_empty() {
                bail!("a phone number is required to log in");
            }
            phone
        }
    };

    normalize_phone(&raw).with_context(|| format!("invalid phone number {raw:?}"))
}

pub(crate) async fn send_code(
    client: &Client,
    phone: &str,
    api_id: i32,
    api_hash: &str,
    lang: &str,
) -> Result<tl::types::auth::SentCode> {
    let request = tl::functions::auth::SendCode {
        phone_number: phone.to_owned(),
        api_id,
        api_hash: api_hash.to_owned(),
        settings: tl::types::CodeSettings {
            allow_flashcall: false,
            current_number: false,
            allow_app_hash: false,
            allow_missed_call: false,
            allow_firebase: false,
            logout_tokens: None,
            token: None,
            app_sandbox: None,
            unknown_number: false,
        }
        .into(),
    };

    let result = client
        .invoke(&request)
        .await
        .map_err(|err| anyhow::anyhow!("{}", describe_login_error(&err, lang)))?;
    unpack_sent_code(result)
}

pub(crate) async fn resend_code(
    client: &Client,
    phone: &str,
    phone_code_hash: &str,
    lang: &str,
) -> Result<tl::types::auth::SentCode> {
    let request = tl::functions::auth::ResendCode {
        phone_number: phone.to_owned(),
        phone_code_hash: phone_code_hash.to_owned(),
        reason: None,
    };

    let result = client
        .invoke(&request)
        .await
        .map_err(|err| anyhow::anyhow!("{}", describe_login_error(&err, lang)))?;
    unpack_sent_code(result)
}

fn unpack_sent_code(result: tl::enums::auth::SentCode) -> Result<tl::types::auth::SentCode> {
    match result {
        tl::enums::auth::SentCode::Code(code) => Ok(code),
        tl::enums::auth::SentCode::Success(_) => bail!("this session is already authorized"),
        tl::enums::auth::SentCode::PaymentRequired(payment) => bail!(
            "Telegram requires payment for this login ({}, {} {})",
            payment.store_product,
            payment.amount,
            payment.currency
        ),
    }
}

enum LoginFailure {
    PasswordRequired,
    InvalidCode,
    Expired,
    SignUpRequired,
    Other(String),
}

async fn sign_in(
    client: &Client,
    session: &Arc<ErasedSession>,
    phone: &str,
    phone_code_hash: &str,
    code: &str,
    lang: &str,
) -> Result<(), LoginFailure> {
    let request = tl::functions::auth::SignIn {
        phone_number: phone.to_owned(),
        phone_code_hash: phone_code_hash.to_owned(),
        phone_code: Some(code.to_owned()),
        email_verification: None,
    };

    match client.invoke(&request).await {
        Ok(tl::enums::auth::Authorization::Authorization(_)) => {
            finish_login(client, session)
                .await
                .map_err(|err| LoginFailure::Other(format!("{err:#}")))?;
            info!(
                "{}",
                l(lang, "Signed in successfully", "Вход выполнен успешно")
            );
            Ok(())
        }
        Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => Err(LoginFailure::SignUpRequired),
        Err(err) if err.is("SESSION_PASSWORD_NEEDED") => Err(LoginFailure::PasswordRequired),
        Err(err) if err.is("PHONE_CODE_INVALID") => Err(LoginFailure::InvalidCode),
        Err(err) if err.is("PHONE_CODE_EXPIRED") => Err(LoginFailure::Expired),
        Err(err) => Err(LoginFailure::Other(describe_login_error(&err, lang))),
    }
}

/// Turn the most common Telegram RPC failures into actionable hints instead of
/// raw `FLOOD_WAIT_86400` strings.
fn describe_login_error(err: &grammers_client::InvocationError, lang: &str) -> String {
    use grammers_client::InvocationError;

    if let InvocationError::Rpc(rpc) = err {
        let ru = is_ru(lang);
        match rpc.name.as_str() {
            "FLOOD_WAIT" => {
                let wait = rpc
                    .value
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_owned());
                return if ru {
                    format!("Лимит Telegram: подожди {wait} с перед повторным запросом кода.")
                } else {
                    format!("Telegram rate limit: wait {wait}s before requesting another code.")
                };
            }
            "PHONE_NUMBER_FLOOD" => {
                return l(
                    lang,
                    "Telegram temporarily blocked code requests for this number. Wait a few hours or sign in through the official app.",
                    "Telegram временно заблокировал запросы кода для этого номера. Подожди несколько часов или войди через официальное приложение.",
                )
                .to_owned();
            }
            "PHONE_NUMBER_BANNED" => {
                return l(
                    lang,
                    "This phone number is banned from Telegram.",
                    "Этот номер заблокирован в Telegram.",
                )
                .to_owned();
            }
            "PHONE_NUMBER_INVALID" => {
                return l(
                    lang,
                    "Telegram rejected this phone number. Check the country code and digits.",
                    "Telegram отклонил этот номер. Проверь код страны и цифры.",
                )
                .to_owned();
            }
            "API_ID_INVALID" | "API_ID_PUBLISHED_FLOOD" => {
                return l(
                    lang,
                    "The api_id/api_hash pair is invalid. Create your own app at https://my.telegram.org/apps.",
                    "Пара api_id/api_hash неверна. Создай своё приложение на https://my.telegram.org/apps.",
                )
                .to_owned();
            }
            _ => {}
        }
    }

    if is_ru(lang) {
        format!("Запрос к Telegram не удался: {err}")
    } else {
        format!("Telegram request failed: {err}")
    }
}

async fn complete_two_factor(client: &Client, lang: &str) -> Result<()> {
    let mut env_password = std::env::var("TG_2FA_PASSWORD")
        .ok()
        .map(|password| password.trim().to_owned())
        .filter(|password| !password.is_empty());

    for attempt in 1..=MAX_PASSWORD_ATTEMPTS {
        let info: tl::types::account::Password = client
            .invoke(&tl::functions::account::GetPassword {})
            .await
            .map_err(|err| anyhow::anyhow!("failed to fetch 2FA information: {err}"))?
            .into();

        let hint = info.hint.clone().unwrap_or_default();
        let prompt = if hint.is_empty() {
            l(
                lang,
                "Two-factor authentication password: ",
                "Пароль двухфакторной аутентификации: ",
            )
            .to_owned()
        } else {
            format!(
                "{} (hint: {hint}): ",
                l(
                    lang,
                    "Two-factor authentication password",
                    "Пароль двухфакторной аутентификации"
                )
            )
        };

        let password = match env_password.take() {
            Some(password) => password,
            None => config::prompt_secret(&prompt)?,
        };

        match client
            .check_password(PasswordToken::new(info), password)
            .await
        {
            Ok(_) => {
                info!(
                    "{}",
                    l(lang, "Signed in successfully", "Вход выполнен успешно")
                );
                return Ok(());
            }
            Err(SignInError::InvalidPassword(_)) => {
                if attempt == MAX_PASSWORD_ATTEMPTS {
                    bail!("too many wrong two-factor passwords");
                }
                println!(
                    "{}",
                    l(
                        lang,
                        "Wrong password, try again.",
                        "Неверный пароль, попробуй ещё раз."
                    )
                );
            }
            Err(err) => bail!("two-factor authentication failed: {err}"),
        }
    }

    bail!("two-factor authentication failed")
}

/// Mirrors the private `Client::complete_login`: persist the self peer and the
/// update state, so the update loop starts from a clean, authorised state.
async fn finish_login(client: &Client, session: &Arc<ErasedSession>) -> Result<()> {
    if let Ok(tl::enums::updates::State::State(state)) =
        client.invoke(&tl::functions::updates::GetState {}).await
    {
        let updates = UpdatesState {
            pts: state.pts,
            qts: state.qts,
            date: state.date,
            seq: state.seq,
            channels: Vec::new(),
        };
        session
            .set_update_state(UpdateState::All(updates))
            .await
            .map_err(|err| anyhow::anyhow!("failed to persist update state: {err}"))?;
    }

    let me = client
        .get_me()
        .await
        .context("failed to fetch the logged-in user")?;
    let peer = me
        .to_ref()
        .await
        .map_err(|err| anyhow::anyhow!("failed to resolve self peer: {err}"))?
        .context("the logged-in user has no access hash")?;

    session
        .cache_peer(PeerInfo::User {
            id: me.id().bare_id_unchecked(),
            auth: Some(peer.auth),
            bot: Some(me.is_bot()),
            is_self: Some(true),
        })
        .await
        .map_err(|err| anyhow::anyhow!("failed to cache self peer: {err}"))?;

    info!(
        "Logged in as {} (@{})",
        me.full_name(),
        me.username().unwrap_or("no username")
    );
    Ok(())
}

pub(crate) fn delivery_hint(
    kind: &tl::enums::auth::SentCodeType,
    phone: &str,
    lang: &str,
) -> String {
    describe_delivery(kind, phone, lang)
}

/// Outcome of submitting a login code for an additional account.
pub(crate) enum CodeOutcome {
    SignedIn,
    PasswordRequired,
}

pub(crate) async fn account_sign_in(
    client: &Client,
    session: &Arc<ErasedSession>,
    phone: &str,
    phone_code_hash: &str,
    code: &str,
) -> Result<CodeOutcome> {
    match sign_in(client, session, phone, phone_code_hash, code, "en").await {
        Ok(()) => Ok(CodeOutcome::SignedIn),
        Err(LoginFailure::PasswordRequired) => Ok(CodeOutcome::PasswordRequired),
        Err(LoginFailure::InvalidCode) => bail!("PHONE_CODE_INVALID"),
        Err(LoginFailure::Expired) => bail!("PHONE_CODE_EXPIRED"),
        Err(LoginFailure::SignUpRequired) => {
            bail!("this phone number has no Telegram account")
        }
        Err(LoginFailure::Other(err)) => bail!("{err}"),
    }
}

pub(crate) async fn account_check_password(
    client: &Client,
    session: &Arc<ErasedSession>,
    password: &str,
) -> Result<()> {
    let info: tl::types::account::Password = client
        .invoke(&tl::functions::account::GetPassword {})
        .await
        .map_err(|err| anyhow::anyhow!("failed to fetch 2FA information: {err}"))?
        .into();

    client
        .check_password(PasswordToken::new(info), password.to_owned())
        .await
        .map_err(|err| anyhow::anyhow!("two-factor authentication failed: {err}"))?;

    finish_login(client, session).await
}

fn print_delivery(sent: &tl::types::auth::SentCode, phone: &str, lang: &str) {
    println!("{}", describe_delivery(&sent.r#type, phone, lang));
    if let Some(timeout) = sent.timeout {
        println!(
            "{}",
            if is_ru(lang) {
                format!("Если код не пришёл за {timeout} с, введи 'resend'.")
            } else {
                format!("If the code does not arrive within {timeout}s, type 'resend'.")
            }
        );
    }
}

fn describe_delivery(kind: &tl::enums::auth::SentCodeType, phone: &str, lang: &str) -> String {
    use tl::enums::auth::SentCodeType as Kind;

    let ru = is_ru(lang);
    match kind {
        Kind::App(_) => {
            if ru {
                "Код отправлен в приложение Telegram — открой чат «Telegram» (777000).".to_owned()
            } else {
                "Code sent to the Telegram app — open the \"Telegram\" chat (777000).".to_owned()
            }
        }
        Kind::Sms(_) | Kind::FirebaseSms(_) | Kind::SmsWord(_) | Kind::SmsPhrase(_) => {
            if ru {
                format!("Код отправлен по SMS на {phone}. Доставка может занять 1–2 минуты.")
            } else {
                format!("Code sent by SMS to {phone}. Delivery can take 1–2 minutes.")
            }
        }
        Kind::Call(_) => {
            if ru {
                format!("Telegram позвонит на {phone} и продиктует код.")
            } else {
                format!("Telegram will call {phone} and dictate the code.")
            }
        }
        Kind::FlashCall(_) => {
            if ru {
                format!("Telegram подтвердит {phone} flash-звонком.")
            } else {
                format!("Telegram will verify {phone} with a flash call.")
            }
        }
        Kind::MissedCall(_) => {
            if ru {
                format!("Telegram позвонит на {phone}; код — последние цифры номера звонящего.")
            } else {
                format!(
                    "Telegram will call {phone}; the code is the last digits of the caller number."
                )
            }
        }
        Kind::EmailCode(email) => {
            if ru {
                format!("Код отправлен на e-mail {}.", email.email_pattern)
            } else {
                format!("Code sent to the login e-mail {}.", email.email_pattern)
            }
        }
        Kind::FragmentSms(fragment) => {
            if ru {
                format!("Код выдаётся через Fragment: {}", fragment.url)
            } else {
                format!("Code is delivered via Fragment: {}", fragment.url)
            }
        }
        Kind::SetUpEmailRequired(_) => {
            if ru {
                "Telegram просит сначала настроить e-mail для входа в официальном приложении."
                    .to_owned()
            } else {
                "Telegram asks you to set up a login e-mail in the official app first.".to_owned()
            }
        }
    }
}

fn l<'a>(lang: &str, en: &'a str, ru: &'a str) -> &'a str {
    if is_ru(lang) {
        ru
    } else {
        en
    }
}

fn is_ru(lang: &str) -> bool {
    let lang = lang.trim().to_lowercase();
    lang.starts_with("ru") || lang.starts_with("uk") || lang.starts_with("be")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_phone_numbers() {
        assert_eq!(
            normalize_phone("+7 (900) 185-37-29").unwrap(),
            "+79001853729"
        );
        assert_eq!(normalize_phone(" 0079001853729 ").unwrap(), "+79001853729");
        assert_eq!(normalize_phone("89001853729").unwrap(), "+79001853729");
        assert!(normalize_phone("890018537").is_err());
        assert!(normalize_phone("+7abc").is_err());
        assert!(normalize_phone("+123").is_err());
        assert!(normalize_phone("").is_err());
    }

    #[test]
    fn encodes_base64url_without_padding() {
        assert_eq!(base64_url_nopad(b""), "");
        assert_eq!(base64_url_nopad(b"f"), "Zg");
        assert_eq!(base64_url_nopad(b"fo"), "Zm8");
        assert_eq!(base64_url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64_url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64_url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64_url_nopad(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_url_nopad(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn describes_delivery_channels() {
        let app =
            tl::enums::auth::SentCodeType::App(tl::types::auth::SentCodeTypeApp { length: 5 });
        let sms =
            tl::enums::auth::SentCodeType::Sms(tl::types::auth::SentCodeTypeSms { length: 5 });
        assert!(describe_delivery(&app, "+79001234567", "en").contains("777000"));
        assert!(describe_delivery(&sms, "+79001234567", "en").contains("SMS"));
        assert!(describe_delivery(&app, "+79001234567", "ru").contains("Telegram"));
    }
}
