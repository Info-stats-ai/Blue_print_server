//! Phase 8 — desktop-loopback OAuth (GitHub + Google), replacing in-panel
//! email/password as the primary sign-in path.
//!
//! Flow: the IDE opens the user's real browser to
//! `GET /auth/{provider}?redirect_port=PORT`, where `PORT` is a one-shot
//! local HTTP server the IDE just bound. This handler redirects to the
//! provider's consent screen; the provider calls back to
//! `GET /auth/{provider}/callback`, which exchanges the code, looks up (or
//! creates) a user by the provider's verified email, issues our own JWT via
//! `auth::create_jwt`, and 302s to `http://localhost:{redirect_port}/?token=...`
//! — the already-listening IDE process on that port picks it up from there.
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
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::AppState;
use crate::auth::create_jwt;
use crate::errors::AppError;

// How long a CSRF nonce stays valid between redirecting to the provider and
// the provider calling back — generous for a human completing a login
// screen, short enough that an abandoned login can't be replayed much later.
const STATE_TTL: Duration = Duration::from_secs(300);

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

    async fn issue(&self, provider: Provider, redirect_port: u16) -> String {
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
            },
        );
        token
    }

    async fn consume(&self, state: &str, provider: Provider) -> Option<u16> {
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
        pending.remove(state).map(|entry| entry.redirect_port)
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
}

/// `GET /auth/{provider}?redirect_port=PORT` — redirects the browser to the
/// provider's consent screen.
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
    let oauth_state = state.oauth.issue(provider, query.redirect_port).await;

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
    let Some(redirect_port) = state.oauth.consume(&query.state, provider).await else {
        return Err(AppError::BadRequest(
            "invalid or expired oauth state".into(),
        ));
    };

    let result = complete_login(&state, provider, query).await;
    Ok(match result {
        // Mirrors `AuthResponse { token, user_id }`, the same shape
        // `/auth/login`/`/auth/register` return directly — the IDE's
        // loopback listener needs both, not just the token.
        Ok((user_id, token)) => Redirect::to(&format!(
            "http://localhost:{redirect_port}/?token={}&user_id={}",
            urlencoding::encode(&token),
            urlencoding::encode(&user_id),
        )),
        Err(error) => Redirect::to(&format!(
            "http://localhost:{redirect_port}/?error={}",
            urlencoding::encode(&error.to_string())
        )),
    })
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
        let token = oauth.issue(Provider::Github, 51234).await;

        assert_eq!(
            oauth.consume(&token, Provider::Google).await,
            None,
            "wrong provider must not match"
        );

        let consumed = oauth.consume(&token, Provider::Github).await;
        assert_eq!(consumed, Some(51234));
        assert_eq!(
            oauth.consume(&token, Provider::Github).await,
            None,
            "a state token must be single-use"
        );
    }
}
