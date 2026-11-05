//! The public website: home, download, docs and the sign-in page the IDE opens.
//!
//! Pages are compiled in, so a deploy can't lose them. Home, download and docs
//! are reached through `preview::fallback`, so they never shadow a path of an
//! app someone is sharing. The sign-in page lives under `/auth`, which is
//! already this server's own.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;

use crate::AppState;
use crate::errors::AppError;
use crate::models::OkResponse;

const CSS: &str = include_str!("../site/style.css");
const NAV: &str = include_str!("../site/nav.html");
const SIM: &str = include_str!("../site/claim-sim.js");
const HOME: &str = include_str!("../site/index.html");
const DOWNLOAD: &str = include_str!("../site/download.html");
const DOCS: &str = include_str!("../site/docs.html");
const SIGNIN: &str = include_str!("../site/signin.html");

const DOC_PAGES: &[&str] = &[
    "quickstart",
    "tutorial",
    "sign-in",
    "solo-and-sync",
    "sessions",
    "chat",
    "tasks",
    "sync",
    "previews",
    "your-ai",
    "claims",
    "conflicts",
    "team-agent",
    "reference-doc",
    "controls",
    "limits",
    "security",
    "troubleshooting",
    "faq",
];

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; \
    style-src 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
    script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; media-src 'self'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

fn html(template: &str) -> Response {
    let body = template.replace("{{CSS}}", CSS).replace("{{NAV}}", NAV).replace("{{SIM}}", SIM);
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        body,
    )
        .into_response()
}

/// The website's page at `path`, for requests no route or shared app claimed.
pub fn page(path: &str) -> Response {
    match path.trim_end_matches('/') {
        "" => html(HOME),
        "/download" => html(DOWNLOAD),
        // One file holds every docs page; its script shows the one in the URL.
        "/docs" => html(DOCS),
        path if path.strip_prefix("/docs/").is_some_and(|slug| DOC_PAGES.contains(&slug)) => html(DOCS),
        _ => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

#[derive(Debug, Deserialize)]
pub struct SigninQuery {
    pub redirect_port: Option<u16>,
    pub public_key: Option<String>,
}

/// `GET /auth/signin`: the page the IDE opens. The provider buttons carry the
/// IDE's port and key on to `oauth::start`. Opened from the website there's no
/// IDE waiting, so it points to the app instead.
pub async fn signin(Query(query): Query<SigninQuery>) -> Response {
    let (from_ide, from_web) = match query.redirect_port {
        Some(port) => {
            let mut params = format!("redirect_port={port}");
            if let Some(public_key) = &query.public_key {
                params.push_str(&format!("&public_key={}", urlencoding::encode(public_key)));
            }
            (params, "hidden")
        }
        None => (String::new(), ""),
    };
    let ide_hidden = if query.redirect_port.is_some() { "" } else { "hidden" };
    html(
        &SIGNIN
            .replace("{{PARAMS}}", &from_ide)
            .replace("{{IDE_HIDDEN}}", ide_hidden)
            .replace("{{WEB_HIDDEN}}", from_web),
    )
}

#[derive(Debug, Deserialize)]
pub struct WaitlistRequest {
    pub email: String,
}

fn looks_like_email(email: &str) -> bool {
    email.len() <= 254
        && !email.contains(char::is_whitespace)
        && email
            .split_once('@')
            .is_some_and(|(name, domain)| !name.is_empty() && domain.contains('.') && !domain.contains('@'))
}

/// `POST /waitlist`. Joining twice looks the same as joining once, so this
/// can't be used to find out who has signed up.
pub async fn join_waitlist(
    State(state): State<AppState>,
    Json(payload): Json<WaitlistRequest>,
) -> Result<Json<OkResponse>, AppError> {
    let email = payload.email.trim().to_lowercase();
    if !looks_like_email(&email) {
        return Err(AppError::BadRequest("enter a valid email address".into()));
    }
    sqlx::query("INSERT OR IGNORE INTO waitlist (email, created_at) VALUES (?, ?)")
        .bind(&email)
        .bind(Utc::now())
        .execute(&state.pool)
        .await?;
    Ok(Json(OkResponse { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plausible_addresses_join_the_waitlist() {
        assert!(looks_like_email("a@b.co"));
        assert!(!looks_like_email("a@b"));
        assert!(!looks_like_email("@b.co"));
        assert!(!looks_like_email("a b@c.co"));
        assert!(!looks_like_email("a@b@c.co"));
    }

    #[test]
    fn every_docs_page_has_a_route_and_unknown_ones_dont() {
        for slug in DOC_PAGES {
            assert!(DOCS.contains(&format!("data-page=\"/docs/{slug}\"")), "{slug}");
            assert_eq!(page(&format!("/docs/{slug}")).status(), StatusCode::OK);
        }
        assert_eq!(page("/docs/nope").status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_signin_page_passes_the_ides_key_on_safely() {
        let response = signin(Query(SigninQuery {
            redirect_port: Some(4242),
            public_key: Some("a\"><script>".to_string()),
        }))
        .await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("/auth/github?redirect_port=4242&public_key=a%22%3E%3Cscript%3E"));
        assert!(body.contains("/auth/google?redirect_port=4242"));
        assert!(!body.contains("a\"><script>"));
    }
}
