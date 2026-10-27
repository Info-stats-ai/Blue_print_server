//! Phase 8 — desktop-loopback OAuth (GitHub + Google), replacing in-panel
//! email/password as the primary sign-in path.
//!
//! Flow: the IDE generates a one-time RSA keypair, opens the user's real
//! browser to `GET /auth/{provider}?redirect_port=PORT&public_key=...`,
//! where `PORT` is a one-shot local HTTP server the IDE just bound. This
//! handler redirects to the provider's consent screen; the provider calls
//! back to `GET /auth/{provider}/callback`, which exchanges the code, looks
//! up (or creates) a user by the provider's verified email, issues our own
//! JWT via `auth::create_jwt`, mints a short single-use opaque exchange
//! code for it (`ExchangeState`), encrypts *that* code with the IDE's
//! public key, and 302s to
//! `http://localhost:{redirect_port}/?encrypted_code=...`. The IDE decrypts
//! the code locally with its private key (which never left its own
//! process) and makes a second, ordinary authenticated call —
//! `POST /auth/exchange`, over normal HTTPS, not through the loopback at
//! all — to trade it for the real token.
//!
//! Why two hops instead of putting the JWT straight in the redirect: a
//! local process that raced to bind `redirect_port` before the IDE did, or
//! that fires its own forged request at an already-bound listener, used to
//! be able to hand the IDE a value of its own choosing (a real local-
//! impersonation risk, not a network one — the CSRF `state` check already
//! stopped a remote attacker from driving the OAuth exchange itself). Now
//! the only thing that ever crosses that racy channel is RSA-OAEP
//! ciphertext of a 64-char opaque code, decryptable only by whichever
//! process holds the matching private key — generated fresh per login
//! attempt, held only in that one IDE process's memory. A local attacker
//! who wins the race just gets ciphertext they can't open; the real JWT
//! only ever travels over the follow-up authenticated HTTPS call the IDE
//! itself initiates. This mirrors Zed's own desktop-loopback sign-in
//! (`crates/client/src/client.rs`, `rpc::auth`) exactly, reusing that same
//! `rpc::auth` keypair/encrypt/decrypt code rather than a second
//! hand-rolled implementation of it.
//!
//! Backward compatible: a caller that omits `public_key` gets the old
//! direct `?token=...&user_id=...` redirect — nothing here requires every
//! caller to have updated at once.
//!
//! Dormant until configured: every handler checks its provider's client
//! id/secret and `PUBLIC_BASE_URL` via env first and returns a clear error
//! instead of building a malformed request — the same degrade-not-crash
//! pattern `agent.rs` already uses for a missing `ANTHROPIC_API_KEY`. A
//! deployment that hasn't registered OAuth apps yet just can't serve these
//! routes; email/password keeps working regardless.
//!
//! Required env vars (set on whichever host runs this server, e.g. Railway):
//! `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, `GOOGLE_CLIENT_ID`,
//! `GOOGLE_CLIENT_SECRET`, `PUBLIC_BASE_URL` (this server's own public URL,
//! used to build the OAuth callback redirect_uri registered with each
//! provider).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::Json;
use chrono::Utc;
use rpc::auth::{EncryptionFormat, PublicKey, random_token};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::AppState;
use crate::auth::create_jwt;
use crate::errors::AppError;
use crate::models::AuthResponse;

// How long a CSRF nonce stays valid between redirecting to the provider and
// the provider calling back — generous for a human completing a login
// screen, short enough that an abandoned login can't be replayed much later.
const STATE_TTL: Duration = Duration::from_secs(300);

// How long a minted exchange code stays redeemable — just long enough for
// the IDE's own loopback listener to receive the redirect, decrypt it, and
// immediately fire the follow-up `/auth/exchange` call. Generous would
// defeat the point: the whole reason this is short-lived and single-use is
// so a leaked/logged code (server access logs, a proxy, browser history on
// the *provider's* side) stops being worth anything almost immediately.
const EXCHANGE_TTL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Github,
    Google,
}

impl Provider {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "github" => Some(Self::Github),
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    fn slug(&self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Google => "google",
        }
    }

    fn client_id_env(&self) -> &'static str {
        match self {
            Self::Github => "GITHUB_CLIENT_ID",
            Self::Google => "GOOGLE_CLIENT_ID",
        }
    }

    fn client_secret_env(&self) -> &'static str {
        match self {
            Self::Github => "GITHUB_CLIENT_SECRET",
            Self::Google => "GOOGLE_CLIENT_SECRET",
        }
    }

    fn authorize_url(&self) -> &'static str {
        match self {
            Self::Github => "https://github.com/login/oauth/authorize",
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
        }
    }

    fn token_url(&self) -> &'static str {
        match self {
            Self::Github => "https://github.com/login/oauth/access_token",
            Self::Google => "https://oauth2.googleapis.com/token",
        }
    }

    fn scope(&self) -> &'static str {
        match self {
            Self::Github => "read:user user:email",
            Self::Google => "openid email profile",
        }
    }
}

struct PendingAuth {
    redirect_port: u16,
    provider: Provider,
    created_at: Instant,
    // Absent for a caller that hasn't adopted the encrypted-exchange flow
    // yet — `callback` falls back to the old direct-token redirect in that
    // case. See this module's doc comment for why present means encrypted.
    public_key: Option<String>,
}

/// The CSRF-nonce -> pending-login map, keyed by a random token embedded as
/// the OAuth `state` parameter. Exists specifically so a malicious page
/// can't forge a callback that redirects a freshly-issued token to a
/// `redirect_port` of its own choosing — `consume` only accepts a state
/// value this server itself issued, for the same provider, within
/// `STATE_TTL`.
#[derive(Clone, Default)]
pub struct OAuthState {
    pending: Arc<RwLock<HashMap<String, PendingAuth>>>,
}

impl OAuthState {
    pub fn new() -> Self {
        Self::default()
    }

    async fn issue(&self, provider: Provider, redirect_port: u16, public_key: Option<String>) -> String {
        let mut pending = self.pending.write().await;
        // Opportunistic cleanup on every issue, so an map of abandoned
        // logins can't grow without bound — there's no separate sweep task
        // for this, unlike the session Hub, since login attempts are rare
        // enough that piggybacking here is sufficient.
        pending.retain(|_, entry| entry.created_at.elapsed() < STATE_TTL);
        let token = Uuid::new_v4().to_string();
        pending.insert(
            token.clone(),
            PendingAuth {
                redirect_port,
                provider,
                created_at: Instant::now(),
                public_key,
            },
        );
        token
    }

    async fn consume(&self, state: &str, provider: Provider) -> Option<(u16, Option<String>)> {
        let mut pending = self.pending.write().await;
        // Only removes the entry on an actual match — a mismatched provider
        // or a lookup against someone else's state must not burn a token
        // that's still valid for its real callback, which an unconditional
        // `remove` would do even on a rejected lookup.
        let matches = pending.get(state).is_some_and(|entry| {
            entry.provider == provider && entry.created_at.elapsed() < STATE_TTL
        });
        if !matches {
            return None;
        }
        pending
            .remove(state)
            .map(|entry| (entry.redirect_port, entry.public_key))
    }
}

struct PendingExchange {
    user_id: String,
    token: String,
    created_at: Instant,
}

/// Single-use, short-lived opaque-code -> (user_id, token) store. The code
/// is what actually crosses the racy local loopback redirect (encrypted);
/// the real JWT only ever leaves this map via the authenticated
/// `POST /auth/exchange` call the IDE makes right after decrypting it. See
/// this module's doc comment for the full reasoning.
#[derive(Clone, Default)]
pub struct ExchangeState {
    pending: Arc<RwLock<HashMap<String, PendingExchange>>>,
}

impl ExchangeState {
    pub fn new() -> Self {
        Self::default()
    }

    async fn issue(&self, user_id: String, token: String) -> String {
        let mut pending = self.pending.write().await;
        pending.retain(|_, entry| entry.created_at.elapsed() < EXCHANGE_TTL);
        let code = random_token();
        pending.insert(
            code.clone(),
            PendingExchange {
                user_id,
                token,
                created_at: Instant::now(),
            },
        );
        code
    }

    async fn consume(&self, code: &str) -> Option<(String, String)> {
        let mut pending = self.pending.write().await;
        let entry = pending.remove(code)?;
        if entry.created_at.elapsed() >= EXCHANGE_TTL {
            return None;
        }
        Some((entry.user_id, entry.token))
    }
}

// These three return `anyhow::Result` (not `AppError`) because they're
// shared by both axum handlers below (which map the error into an
// `AppError` themselves) and `fetch_verified_email`/`complete_login`, which
// are plain async functions returning `anyhow::Result` — `AppError` has no
// `std::error::Error` impl, so it can't convert into `anyhow::Error` via
// `?` the way every other error type here does.
fn callback_url(provider: Provider) -> anyhow::Result<String> {
    let base = std::env::var("PUBLIC_BASE_URL").context("PUBLIC_BASE_URL is not set")?;
    Ok(format!(
        "{}/auth/{}/callback",
        base.trim_end_matches('/'),
        provider.slug()
    ))
}

fn require_client_id(provider: Provider) -> anyhow::Result<String> {
    std::env::var(provider.client_id_env())
        .with_context(|| format!("{} is not set", provider.client_id_env()))
}

fn require_client_secret(provider: Provider) -> anyhow::Result<String> {
    std::env::var(provider.client_secret_env())
        .with_context(|| format!("{} is not set", provider.client_secret_env()))
}

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    pub redirect_port: u16,
    // Base64url-encoded DER public key (`String::try_from(rpc::auth::PublicKey)`).
    // Absent falls back to the old unencrypted redirect — see this module's
    // doc comment.
    pub public_key: Option<String>,
}

/// `GET /auth/{provider}?redirect_port=PORT&public_key=...` — redirects the
/// browser to the provider's consent screen.
pub async fn start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<StartQuery>,
) -> Result<Redirect, AppError> {
    let provider = Provider::parse(&provider)
        .ok_or_else(|| AppError::BadRequest(format!("unknown oauth provider: {provider}")))?;
    let client_id =
        require_client_id(provider).map_err(|error| AppError::Internal(error.to_string()))?;
    let redirect_uri =
        callback_url(provider).map_err(|error| AppError::Internal(error.to_string()))?;
    let oauth_state = state
        .oauth
        .issue(provider, query.redirect_port, query.public_key)
        .await;

    let url = format!(
        "{}?client_id={}&redirect_uri={}&scope={}&state={}&response_type=code",
        provider.authorize_url(),
        urlencoding::encode(&client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(provider.scope()),
        urlencoding::encode(&oauth_state),
    );
    Ok(Redirect::to(&url))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: String,
    pub error: Option<String>,
}

/// `GET /auth/{provider}/callback` — exchanges `code` for an access token,
/// resolves the user's verified email from the provider, finds-or-creates a
/// `users` row for it, issues our JWT, and redirects to the IDE's loopback
/// listener with it. Every failure path redirects back to that same
/// loopback port with an `error` query param instead of rendering a raw
/// error page server-side, since nobody but the IDE's one-shot listener
/// will ever see this response — there's no UI here to show an error in.
pub async fn callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Result<Redirect, AppError> {
    let provider = Provider::parse(&provider)
        .ok_or_else(|| AppError::BadRequest(format!("unknown oauth provider: {provider}")))?;
    let Some((redirect_port, public_key)) = state.oauth.consume(&query.state, provider).await
    else {
        return Err(AppError::BadRequest(
            "invalid or expired oauth state".into(),
        ));
    };

    let result = complete_login(&state, provider, query).await;
    let (user_id, token) = match result {
        Ok(pair) => pair,
        Err(error) => {
            return Ok(Redirect::to(&format!(
                "http://localhost:{redirect_port}/?error={}",
                urlencoding::encode(&error.to_string())
            )));
        }
    };

    let Some(public_key_str) = public_key else {
        // Caller never asked for encryption — old direct-token redirect.
        return Ok(Redirect::to(&format!(
            "http://localhost:{redirect_port}/?token={}&user_id={}",
            urlencoding::encode(&token),
            urlencoding::encode(&user_id),
        )));
    };

    // New, secure path: mint a short-lived single-use code for this
    // (user_id, token), encrypt *that* with the IDE's public key, and send
    // only the ciphertext across the loopback — see this module's doc
    // comment. A malformed/unparseable public_key is an error redirect, not
    // a silent downgrade to the plaintext path: a client that asked for
    // encryption should never silently not get it.
    let public_key = match PublicKey::try_from(public_key_str) {
        Ok(key) => key,
        Err(error) => {
            return Ok(Redirect::to(&format!(
                "http://localhost:{redirect_port}/?error={}",
                urlencoding::encode(&format!("invalid public_key: {error}"))
            )));
        }
    };
    let code = state.exchange.issue(user_id, token).await;
    let encrypted_code = match public_key.encrypt_string(&code, EncryptionFormat::V1) {
        Ok(encrypted) => encrypted,
        Err(error) => {
            return Ok(Redirect::to(&format!(
                "http://localhost:{redirect_port}/?error={}",
                urlencoding::encode(&format!("failed to encrypt exchange code: {error}"))
            )));
        }
    };
    Ok(Redirect::to(&format!(
        "http://localhost:{redirect_port}/?encrypted_code={}",
        urlencoding::encode(&encrypted_code),
    )))
}

#[derive(Debug, Deserialize)]
pub struct ExchangeRequest {
    pub code: String,
}

/// `POST /auth/exchange` — trades a short-lived single-use code (decrypted
/// locally by the IDE from the encrypted OAuth callback redirect) for the
/// real token, over an ordinary authenticated HTTPS call instead of the
/// racy local loopback. See this module's doc comment for why this hop
/// exists at all.
pub async fn exchange(
    State(state): State<AppState>,
    Json(payload): Json<ExchangeRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let Some((user_id, token)) = state.exchange.consume(&payload.code).await else {
        return Err(AppError::BadRequest(
            "invalid or expired exchange code".into(),
        ));
    };
    Ok(Json(AuthResponse { token, user_id }))
}

async fn complete_login(
    state: &AppState,
    provider: Provider,
    query: CallbackQuery,
) -> anyhow::Result<(String, String)> {
    if let Some(error) = query.error {
        anyhow::bail!("{provider:?} declined: {error}");
    }
    let code = query
        .code
        .ok_or_else(|| anyhow::anyhow!("missing authorization code"))?;
    let email = fetch_verified_email(&state.http_client, provider, &code).await?;
    let user_id = find_or_create_user(state, &email).await?;
    let token =
        create_jwt(&user_id, &state.jwt_secret).map_err(|error| anyhow::anyhow!("{error:?}"))?;
    Ok((user_id, token))
}

/// Exchanges `code` for an access token, then calls the provider's own
/// userinfo endpoint with it to get a verified email — never trusts an
/// unverified or client-supplied email, since the whole point of OAuth here
/// is that the *provider* vouches for the identity.
async fn fetch_verified_email(client: &reqwest::Client, provider: Provider, code: &str) -> anyhow::Result<String> {
    let client_id = require_client_id(provider)?;
    let client_secret = require_client_secret(provider)?;
    let redirect_uri = callback_url(provider)?;

    let token_response = client
        .post(provider.token_url())
        .header("accept", "application/json")
        .form(&[
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    let access_token = token_response
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("token exchange response had no access_token"))?;

    match provider {
        Provider::Github => fetch_github_email(client, access_token).await,
        Provider::Google => fetch_google_email(client, access_token).await,
    }
}

async fn fetch_github_email(
    client: &reqwest::Client,
    access_token: &str,
) -> anyhow::Result<String> {
    // GitHub's `/user` endpoint omits email when the account's email is
    // private, which is common — `/user/emails` is the only endpoint
    // guaranteed to return it (given the `user:email` scope requested in
    // `Provider::scope`), each entry marked verified/primary or not.
    #[derive(Deserialize)]
    struct GithubEmail {
        email: String,
        primary: bool,
        verified: bool,
    }

    let emails: Vec<GithubEmail> = client
        .get("https://api.github.com/user/emails")
        .header("authorization", format!("Bearer {access_token}"))
        .header("user-agent", "i-ide-blueprint-server")
        .header("accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    emails
        .into_iter()
        .find(|email| email.primary && email.verified)
        .map(|email| email.email)
        .ok_or_else(|| anyhow::anyhow!("GitHub account has no verified primary email"))
}

async fn fetch_google_email(
    client: &reqwest::Client,
    access_token: &str,
) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct GoogleUserInfo {
        email: String,
        email_verified: bool,
    }

    let info: GoogleUserInfo = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .header("authorization", format!("Bearer {access_token}"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    if !info.email_verified {
        anyhow::bail!("Google account email is not verified");
    }
    Ok(info.email)
}

/// Looks up a user by email, creating one if this is their first OAuth
/// login. The created row's `password_hash` is an unusable random value —
/// not a security feature on its own, just guaranteeing an OAuth-only
/// account can never coincidentally match a real bcrypt hash and can never
/// be logged into via the password endpoint, since no password produces it.
async fn find_or_create_user(state: &AppState, email: &str) -> anyhow::Result<String> {
    if let Some((id,)) = sqlx::query_as::<_, (String,)>("SELECT id FROM users WHERE email = ?")
        .bind(email)
        .fetch_optional(&state.pool)
        .await?
    {
        return Ok(id);
    }

    let id = Uuid::new_v4().to_string();
    let unusable_password_hash = format!("oauth:{}", Uuid::new_v4());
    let now = Utc::now();
    sqlx::query("INSERT INTO users (id, email, password_hash, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(email)
        .bind(&unusable_password_hash)
        .bind(now)
        .execute(&state.pool)
        .await?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_providers_only() {
        assert_eq!(Provider::parse("github"), Some(Provider::Github));
        assert_eq!(Provider::parse("google"), Some(Provider::Google));
        assert_eq!(Provider::parse("facebook"), None);
    }

    #[tokio::test]
    async fn state_round_trips_and_is_single_use() {
        let oauth = OAuthState::new();
        let token = oauth
            .issue(Provider::Github, 51234, Some("pub-key".to_string()))
            .await;

        assert_eq!(
            oauth.consume(&token, Provider::Google).await,
            None,
            "wrong provider must not match"
        );

        let consumed = oauth.consume(&token, Provider::Github).await;
        assert_eq!(consumed, Some((51234, Some("pub-key".to_string()))));
        assert_eq!(
            oauth.consume(&token, Provider::Github).await,
            None,
            "a state token must be single-use"
        );
    }

    #[tokio::test]
    async fn exchange_code_round_trips_and_is_single_use() {
        let exchange = ExchangeState::new();
        let code = exchange
            .issue("user-1".to_string(), "jwt-xyz".to_string())
            .await;

        let consumed = exchange.consume(&code).await;
        assert_eq!(consumed, Some(("user-1".to_string(), "jwt-xyz".to_string())));
        assert_eq!(
            exchange.consume(&code).await,
            None,
            "an exchange code must be single-use"
        );
    }

    #[test]
    fn encrypted_exchange_code_decrypts_to_the_original() {
        // The actual end-to-end shape of the fix: a client-generated
        // keypair, a code encrypted server-side with the public half,
        // decrypted client-side with the private half — matching exactly
        // what `callback` does server-side and what the IDE's
        // `authenticate_with_browser` does on receiving it.
        let (public, private) = rpc::auth::keypair().unwrap();
        let public_string = String::try_from(public).unwrap();

        let public = PublicKey::try_from(public_string).unwrap();
        let code = random_token();
        let encrypted = public.encrypt_string(&code, EncryptionFormat::V1).unwrap();
        assert_ne!(encrypted, code);

        let decrypted = private.decrypt_string(&encrypted).unwrap();
        assert_eq!(decrypted, code);
    }
}
