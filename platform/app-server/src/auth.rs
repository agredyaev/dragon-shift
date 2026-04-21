//! Cookie-based account authentication.
//!
//! Introduced in the session-4 refactor to replace the implicit host-cookie
//! created by `POST /api/workshops`. The new model: players sign in once via
//! `POST /api/auth/signin` (create-or-login semantics), receive a signed
//! `ds_session` cookie carrying their `account_id`, and subsequent handlers
//! extract the authenticated account via the [`AccountSession`] extractor.
//!
//! Signing is performed by `axum_extra::extract::cookie::SignedCookieJar`,
//! keyed off [`crate::app::AppConfig::cookie_key`]. The key is threaded into
//! the extractor via a `FromRef<AppState> for Key` impl in `app.rs`.

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use chrono::Utc;
use domain::Account;
use persistence::{AccountRecord, PersistenceError};
use protocol::{AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD, AccountProfile, AuthRequest, AuthResponse};
use security::{PasswordHashError, hash_password, verify_password};
use serde_json::json;
use std::convert::Infallible;
use std::sync::LazyLock;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{MaybeConnectInfo, client_key, is_rate_limited};

/// Name of the signed session cookie. Single source of truth.
pub(crate) const SESSION_COOKIE_NAME: &str = "ds_session";

/// Pre-computed argon2id hash used solely to equalise latency on the
/// unknown-name branch of signin. Without this, the lookup-miss path is
/// observably faster than the lookup-hit + wrong-password path (~60ms
/// difference), giving an attacker a trivial oracle for enumerating
/// registered account names.
///
/// The plaintext that produced this hash is irrelevant — we never compare
/// against a real user-supplied password; we just want `verify_password` to
/// do the same amount of work it does on the known-name branch before we
/// fall through to the create path. `LazyLock` amortises the hashing cost
/// across the process lifetime so the first signin isn't penalised.
static TIMING_DUMMY_HASH: LazyLock<String> = LazyLock::new(|| {
    // "timing-equaliser-not-a-real-password" — contents are arbitrary.
    hash_password("timing-equaliser-not-a-real-password")
        .expect("hashing a constant dummy password must succeed at startup")
});

/// Run `hash_password` on a blocking thread so we never stall the Tokio
/// runtime. argon2id with the configured parameters takes ~60ms of pure
/// CPU; running it inline would block a worker thread from polling WS
/// frames, HTTP requests, and timers for that duration.
async fn hash_password_blocking(password: String) -> Result<String, PasswordHashError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .unwrap_or(Err(PasswordHashError::HashFailure))
}

/// Run `verify_password` on a blocking thread (see `hash_password_blocking`).
async fn verify_password_blocking(
    password: String,
    stored_hash: String,
) -> Result<bool, PasswordHashError> {
    tokio::task::spawn_blocking(move || verify_password(&password, &stored_hash))
        .await
        .unwrap_or(Err(PasswordHashError::MalformedHash))
}

/// Build the server-set session cookie for the given account id.
///
/// Centralises the cookie attributes so signin, refresh, and any future
/// re-issue paths stay consistent. `Secure` is gated on `is_production` so
/// local HTTP development still works.
pub(crate) fn build_session_cookie(account_id: String, is_production: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, account_id);
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(is_production);
    // Spec: 14-day max-age so the cookie survives browser restarts.
    cookie.set_max_age(time::Duration::days(14));
    cookie
}

/// Build the cookie used to clear the session on logout.
///
/// We build a cookie with matching `path` / `same_site` / `secure` attributes
/// so browsers actually replace the existing entry. `SignedCookieJar::remove`
/// will convert this into an expired cookie with an empty value.
pub(crate) fn build_logout_cookie(is_production: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    cookie.set_http_only(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_path("/");
    cookie.set_secure(is_production);
    cookie
}

/// Authenticated account context extracted from a signed session cookie.
///
/// Handlers that require an account should take `AccountSession` as an
/// argument; a missing or tampered cookie yields `401 Unauthorized`, and a
/// cookie referencing a deleted account yields `401` as well (with the
/// offending cookie cleared on the response).
#[derive(Debug, Clone)]
pub(crate) struct AccountSession {
    pub(crate) account: Account,
}

impl FromRequestParts<AppState> for AccountSession {
    type Rejection = AuthRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // `SignedCookieJar` reads the request's Cookie header and validates
        // signatures against the key pulled from AppState via FromRef.
        let jar =
            SignedCookieJar::<Key>::from_request_parts(parts, state)
                .await
                .map_err(|_: Infallible| AuthRejection::Unauthenticated)?;

        let Some(cookie) = jar.get(SESSION_COOKIE_NAME) else {
            return Err(AuthRejection::Unauthenticated);
        };
        let account_id = cookie.value().to_string();
        if account_id.is_empty() {
            return Err(AuthRejection::Unauthenticated);
        }

        match state.store.find_account_by_id(&account_id).await {
            Ok(Some(record)) => Ok(AccountSession {
                account: account_from_record(&record),
            }),
            Ok(None) => Err(AuthRejection::UnknownAccount {
                key: state.config.cookie_key.clone(),
                is_production: state.config.is_production,
            }),
            Err(error) => {
                tracing::error!(%error, "account lookup failed during auth");
                Err(AuthRejection::Internal)
            }
        }
    }
}

/// Rejection produced by the `AccountSession` extractor.
///
/// We model "cookie missing/invalid" and "cookie references deleted account"
/// separately so the latter can clear the stale cookie on its way out — this
/// keeps the client from repeatedly hitting authenticated endpoints with a
/// cookie that will never resolve.
#[derive(Debug)]
pub(crate) enum AuthRejection {
    Unauthenticated,
    /// Cookie was valid but references a deleted / nonexistent account.
    /// Carries the signing key + production flag so the response can issue a
    /// proper Set-Cookie that clears the stale session.
    UnknownAccount { key: Key, is_production: bool },
    Internal,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        match self {
            AuthRejection::Unauthenticated => (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "not authenticated" })),
            )
                .into_response(),
            AuthRejection::UnknownAccount { key, is_production } => {
                // Build a jar with the real key so the signed removal cookie
                // matches what the browser holds.
                let jar = SignedCookieJar::new(key);
                let jar = jar.remove(build_logout_cookie(is_production));
                (
                    StatusCode::UNAUTHORIZED,
                    jar,
                    Json(json!({ "error": "account no longer exists" })),
                )
                    .into_response()
            }
            AuthRejection::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "internal error" })),
            )
                .into_response(),
        }
    }
}

/// Map a persisted `AccountRecord` to the domain `Account` (strips hash).
pub(crate) fn account_from_record(record: &AccountRecord) -> Account {
    Account {
        id: record.id.clone(),
        hero: record.hero.clone(),
        name: record.name.clone(),
        created_at: chrono::DateTime::parse_from_rfc3339(&record.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    }
}

fn account_profile(account: &Account) -> AccountProfile {
    AccountProfile {
        id: account.id.clone(),
        hero: account.hero.clone(),
        name: account.name.clone(),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `POST /api/auth/signin` — create-or-login.
///
/// Semantics (locked in session 4 planning):
/// - name free → create new account, hash password, return 201.
/// - name exists + password matches → login, touch last_login, return 200.
/// - name exists + password mismatch → 401 with structured error code
///   `name_taken_wrong_password` so the client can render "This name is
///   already registered — enter the correct password or choose a different
///   name." (spec `refactor.md:50`). Any other auth 401 uses the generic
///   `invalid_credentials` code so enumeration surface is unchanged.
///
/// Validation rules (MVP; tightened in a later checkpoint if needed):
/// - hero: 1..=64 chars, trimmed non-empty.
/// - name: 1..=64 chars, trimmed non-empty.
/// - password: 8..=256 chars.
pub(crate) async fn signin(
    State(state): State<AppState>,
    connect_info: MaybeConnectInfo,
    headers: HeaderMap,
    jar: SignedCookieJar<Key>,
    Json(request): Json<AuthRequest>,
) -> Response {
    let hero = request.hero.trim().to_string();
    let name = request.name.trim().to_string();
    let password = request.password;

    if hero.is_empty() || hero.chars().count() > 64 {
        return bad_request("hero must be 1-64 characters");
    }
    if name.is_empty() || name.chars().count() > 64 {
        return bad_request("name must be 1-64 characters");
    }
    if password.chars().count() < 8 || password.chars().count() > 256 {
        return bad_request("password must be 8-256 characters");
    }

    let ip_key = client_key(&state, connect_info, &headers);
    let is_production = state.config.is_production;

    // Case-insensitive lookup. If found → login branch; if absent → create.
    match state.store.find_account_by_name_lower(&name).await {
        Ok(Some(record)) => {
            // Login rate limit: 10/min/IP.
            if is_rate_limited(&state.login_limiter, &ip_key).await {
                return too_many_requests();
            }
            // Login path. Verify runs on a blocking thread; see
            // `verify_password_blocking` for rationale.
            match verify_password_blocking(password.clone(), record.password_hash.clone()).await {
                Ok(true) => {
                    let now = Utc::now().to_rfc3339();
                    if let Err(error) = state.store.touch_last_login(&record.id, &now).await {
                        // Non-fatal: log and continue issuing the cookie.
                        tracing::warn!(%error, account_id = %record.id, "touch_last_login failed");
                    }
                    let account = account_from_record(&record);
                    let jar = jar.add(build_session_cookie(record.id.clone(), is_production));
                    let body = AuthResponse {
                        account: account_profile(&account),
                        created: false,
                    };
                    (StatusCode::OK, jar, Json(body)).into_response()
                }
                Ok(false) => name_taken_wrong_password(),
                Err(error) => {
                    tracing::error!(%error, account_id = %record.id, "verify_password failed");
                    internal_error()
                }
            }
        }
        Ok(None) => {
            // Signup rate limit: 5/min/IP.
            if is_rate_limited(&state.signup_limiter, &ip_key).await {
                return too_many_requests();
            }
            // Equalise timing with the known-name-wrong-password branch so
            // an attacker can't distinguish "name exists" from "name free"
            // by measuring response latency. We don't care about the result;
            // we just need argon2 to burn the same CPU budget.
            let _ = verify_password_blocking(password.clone(), TIMING_DUMMY_HASH.clone()).await;

            // Create path.
            let hash = match hash_password_blocking(password.clone()).await {
                Ok(h) => h,
                Err(PasswordHashError::InvalidParams | PasswordHashError::HashFailure) => {
                    tracing::error!("hash_password failed during signin");
                    return internal_error();
                }
                Err(PasswordHashError::MalformedHash) => {
                    // Shouldn't happen from hash_password, but cover the arm.
                    tracing::error!("hash_password returned MalformedHash");
                    return internal_error();
                }
            };
            let now = Utc::now().to_rfc3339();
            let record = AccountRecord {
                id: Uuid::new_v4().to_string(),
                hero: hero.clone(),
                name: name.clone(),
                password_hash: hash,
                created_at: now.clone(),
                updated_at: now,
                last_login_at: None,
            };
            match state.store.insert_account(&record).await {
                Ok(()) => {
                    let account = account_from_record(&record);
                    let jar = jar.add(build_session_cookie(record.id.clone(), is_production));
                    let body = AuthResponse {
                        account: account_profile(&account),
                        created: true,
                    };
                    (StatusCode::CREATED, jar, Json(body)).into_response()
                }
                Err(PersistenceError::DuplicateAccountName) => {
                    // Race: another request inserted the same name between
                    // our lookup and insert. Treat as "name taken".
                    (
                        StatusCode::CONFLICT,
                        Json(json!({ "error": "account name already taken" })),
                    )
                        .into_response()
                }
                Err(error) => {
                    tracing::error!(%error, "insert_account failed");
                    internal_error()
                }
            }
        }
        Err(error) => {
            tracing::error!(%error, "find_account_by_name_lower failed");
            internal_error()
        }
    }
}

/// `POST /api/auth/logout` — clear the session cookie.
///
/// Always returns 204 No Content; idempotent regardless of whether a valid
/// cookie was present.
pub(crate) async fn logout(
    State(state): State<AppState>,
    jar: SignedCookieJar<Key>,
) -> Response {
    let jar = jar.remove(build_logout_cookie(state.config.is_production));
    (StatusCode::NO_CONTENT, jar).into_response()
}

fn bad_request(message: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": message }))).into_response()
}

fn name_taken_wrong_password() -> Response {
    // Structured error body: distinct `error` code + human-readable `message`.
    // The client (components/sign_in.rs) matches on `error` to render the
    // spec copy from refactor.md:50. Any future generic 401 from this
    // handler should use a different code (e.g. `invalid_credentials`) so
    // we never leak "name exists" on branches other than the known-name
    // + wrong-password path that already distinguishes itself via argon2.
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": AUTH_ERR_NAME_TAKEN_WRONG_PASSWORD,
            "message": "That name is already registered. Enter the correct password or choose a different name.",
        })),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "internal error" })),
    )
        .into_response()
}

fn too_many_requests() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({ "error": "too many requests, please slow down" })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/accounts/me
// ---------------------------------------------------------------------------

/// Returns the authenticated account's public profile.
/// First real consumer of the `AccountSession` extractor.
pub(crate) async fn accounts_me(session: AccountSession) -> Json<AccountProfile> {
    Json(account_profile(&session.account))
}
