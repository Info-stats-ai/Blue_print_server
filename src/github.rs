//! The GitHub App: when a pull request is opened (or updated) in a repo a
//! session is linked to, it posts one comment with that session's numbers —
//! conflicts caught, edits merged live — and keeps it current on later pushes.
//!
//! Configured by `GITHUB_APP_ID`, `GITHUB_APP_PRIVATE_KEY` (the app's PEM key;
//! literal `\n` sequences are accepted) and `GITHUB_WEBHOOK_SECRET`. Without
//! them the webhook answers 503 and nothing else changes.
//!
//! A PR only ever gets numbers from a session linked to its own repo: a
//! session named in the PR description is used only if it's linked there, so
//! nobody can pull another team's session into their PR. The comment links
//! back to the session only on private repos, since a session id is enough to
//! join it.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sqlx::SqlitePool;
use std::sync::Arc;

use crate::AppState;
use crate::auth::AuthUser;
use crate::errors::AppError;

pub struct GithubApp {
    app_id: String,
    key: EncodingKey,
    webhook_secret: String,
    api_base: String,
    /// The server's own public URL, for the join link in comments.
    public_url: Option<String>,
}

impl GithubApp {
    /// Reads the app's settings from the environment; `None` if any is missing.
    pub fn from_env() -> Option<Arc<Self>> {
        let app_id = std::env::var("GITHUB_APP_ID").ok().filter(|value| !value.trim().is_empty())?;
        let pem = std::env::var("GITHUB_APP_PRIVATE_KEY").ok()?.replace("\\n", "\n");
        let webhook_secret = std::env::var("GITHUB_WEBHOOK_SECRET").ok().filter(|value| !value.is_empty())?;
        let key = match EncodingKey::from_rsa_pem(pem.trim().as_bytes()) {
            Ok(key) => key,
            Err(error) => {
                eprintln!("github: GITHUB_APP_PRIVATE_KEY isn't a usable RSA key: {error}");
                return None;
            }
        };
        Some(Arc::new(Self {
            app_id: app_id.trim().to_string(),
            key,
            webhook_secret,
            api_base: std::env::var("GITHUB_API_URL").unwrap_or_else(|_| "https://api.github.com".to_string()),
            public_url: public_url_from_env(),
        }))
    }

    #[cfg(test)]
    fn for_test(pem: &str, secret: &str, api_base: String) -> Self {
        Self {
            app_id: "12345".to_string(),
            key: EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            webhook_secret: secret.to_string(),
            api_base,
            public_url: Some("https://blueprint.example.test".to_string()),
        }
    }

    /// A short-lived JWT identifying the app itself.
    fn app_jwt(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct Claims<'a> {
            iat: i64,
            exp: i64,
            iss: &'a str,
        }
        let now = chrono::Utc::now().timestamp();
        jsonwebtoken::encode(
            &Header::new(Algorithm::RS256),
            &Claims {
                // GitHub allows for some clock drift.
                iat: now - 60,
                exp: now + 9 * 60,
                iss: &self.app_id,
            },
            &self.key,
        )
        .map_err(|error| format!("signing the app token failed: {error}"))
    }
}

/// Railway sets `RAILWAY_PUBLIC_DOMAIN`; `PUBLIC_URL` overrides it.
fn public_url_from_env() -> Option<String> {
    std::env::var("PUBLIC_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .or_else(|| {
            std::env::var("RAILWAY_PUBLIC_DOMAIN")
                .ok()
                .filter(|domain| !domain.is_empty())
                .map(|domain| format!("https://{domain}"))
        })
        .map(|url| url.trim_end_matches('/').to_string())
}

/// Whether `signature` (GitHub's `X-Hub-Signature-256`, "sha256=<hex>") is
/// this body signed with `secret`. Compared in constant time.
pub fn signature_is_valid(secret: &str, body: &[u8], signature: &str) -> bool {
    let Some(hex) = signature.strip_prefix("sha256=") else {
        return false;
    };
    let Some(expected) = decode_hex(hex) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// A session id written in a PR description, as a join link
/// (".../join/<id>" or "i-ide://i-mode/join/<id>").
pub fn session_id_in(text: &str) -> Option<String> {
    let start = text.find("/join/")? + "/join/".len();
    let id: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '-')
        .collect();
    (id.len() >= 8).then_some(id)
}

/// `owner/name`, the way GitHub writes a repo's full name.
pub fn is_repo_name(repo: &str) -> bool {
    let valid = |part: &str| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((owner, name)) if valid(owner) && valid(name) && !name.contains('/'))
}

#[derive(Debug, Default, PartialEq)]
pub struct SessionTotals {
    pub name: String,
    pub conflicts_caught: i64,
    pub agent_collisions: i64,
    pub divergences: i64,
    pub lane_warnings: i64,
    pub live_merges: i64,
    pub people: i64,
}

async fn session_totals(pool: &SqlitePool, session_id: &str) -> Result<SessionTotals, sqlx::Error> {
    let name: Option<(String,)> = sqlx::query_as("SELECT name FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT kind, SUM(count) FROM conflict_daily WHERE session_id = ? GROUP BY kind")
            .bind(session_id)
            .fetch_all(pool)
            .await?;
    let people: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM session_members WHERE session_id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await?;
    let mut totals = SessionTotals {
        name: name.map(|(name,)| name).unwrap_or_default(),
        people: people.0,
        ..SessionTotals::default()
    };
    for (kind, count) in rows {
        match kind.as_str() {
            crate::stats::AGENT_COLLISION => totals.agent_collisions = count,
            crate::stats::DIVERGENCE => totals.divergences = count,
            crate::stats::LANE_WARNING => totals.lane_warnings = count,
            crate::stats::MERGE => totals.live_merges = count,
            _ => {}
        }
    }
    totals.conflicts_caught = totals.agent_collisions + totals.divergences + totals.lane_warnings;
    Ok(totals)
}

fn count_of(count: i64, singular: &str, plural: &str) -> String {
    format!("{count} {}", if count == 1 { singular } else { plural })
}

/// The PR comment. `join_url` is only given for private repos.
pub fn comment_body(totals: &SessionTotals, join_url: Option<&str>) -> String {
    let mut body = format!(
        "**Built in an I session: {}** ({})\n\n\
         | | |\n|---|---|\n\
         | Conflicts caught before they landed | **{}** |\n\
         | Agent collisions (two agents, same function) | {} |\n\
         | Divergences from the shared reference | {} |\n\
         | Out-of-lane edits | {} |\n\
         | Concurrent edits merged live, nothing overwritten | {} |\n",
        totals.name,
        count_of(totals.people, "person", "people"),
        totals.conflicts_caught,
        totals.agent_collisions,
        totals.divergences,
        totals.lane_warnings,
        totals.live_merges,
    );
    if let Some(url) = join_url {
        body.push_str(&format!("\n[Open the session in I]({url}) to see why this code was written this way.\n"));
    }
    body.push_str("\n<sub>Posted by the I GitHub App. Updated on every push.</sub>");
    body
}

#[derive(Deserialize)]
struct PullRequestEvent {
    action: String,
    number: i64,
    pull_request: PullRequest,
    repository: Repository,
    installation: Option<Installation>,
}

#[derive(Deserialize)]
struct PullRequest {
    #[serde(default)]
    body: Option<String>,
}

#[derive(Deserialize)]
struct Repository {
    full_name: String,
    #[serde(default)]
    private: bool,
}

#[derive(Deserialize)]
struct Installation {
    id: i64,
}

/// `POST /github/webhook`.
pub async fn webhook(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(app) = state.github.clone() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "The GitHub App isn't configured on this server.").into_response();
    };
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !signature_is_valid(&app.webhook_secret, &body, signature) {
        return (StatusCode::UNAUTHORIZED, "Bad signature.").into_response();
    }
    let event = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match event {
        "ping" => (StatusCode::OK, "pong").into_response(),
        "pull_request" => match serde_json::from_slice::<PullRequestEvent>(&body) {
            Ok(event) => match handle_pull_request(&state, &app, event).await {
                Ok(outcome) => (StatusCode::OK, outcome).into_response(),
                Err(error) => {
                    eprintln!("github: {error}");
                    (StatusCode::BAD_GATEWAY, error).into_response()
                }
            },
            Err(error) => (StatusCode::BAD_REQUEST, format!("Unexpected payload: {error}")).into_response(),
        },
        _ => (StatusCode::OK, "ignored").into_response(),
    }
}

async fn handle_pull_request(state: &AppState, app: &GithubApp, event: PullRequestEvent) -> Result<String, String> {
    if !matches!(
        event.action.as_str(),
        "opened" | "reopened" | "synchronize" | "edited" | "ready_for_review"
    ) {
        return Ok("ignored".to_string());
    }
    let repo = event.repository.full_name;
    let Some(installation) = event.installation else {
        return Err("the event has no installation id".to_string());
    };
    let named = event.pull_request.body.as_deref().and_then(session_id_in);
    let session: Option<(String,)> = match named {
        Some(session_id) => sqlx::query_as("SELECT id FROM sessions WHERE id = ? AND github_repo = ?")
            .bind(session_id)
            .bind(&repo)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| error.to_string())?,
        None => None,
    };
    let session = match session {
        Some(session) => Some(session),
        None => sqlx::query_as("SELECT id FROM sessions WHERE github_repo = ? ORDER BY created_at DESC LIMIT 1")
            .bind(&repo)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| error.to_string())?,
    };
    let Some((session_id,)) = session else {
        return Ok(format!("no session is linked to {repo}"));
    };

    let totals = session_totals(&state.pool, &session_id)
        .await
        .map_err(|error| error.to_string())?;
    let join_url = event
        .repository
        .private
        .then(|| app.public_url.as_ref().map(|url| format!("{url}/join/{session_id}")))
        .flatten();
    let body = comment_body(&totals, join_url.as_deref());

    let token = installation_token(app, &state.http_client, installation.id).await?;
    let existing: Option<(i64,)> =
        sqlx::query_as("SELECT comment_id FROM pr_comments WHERE repo = ? AND pr_number = ?")
            .bind(&repo)
            .bind(event.number)
            .fetch_optional(&state.pool)
            .await
            .map_err(|error| error.to_string())?;
    let client = &state.http_client;
    let request = match existing {
        Some((comment_id,)) => client.patch(format!("{}/repos/{repo}/issues/comments/{comment_id}", app.api_base)),
        None => client.post(format!("{}/repos/{repo}/issues/{}/comments", app.api_base, event.number)),
    };
    let response = github_headers(request, &token)
        .json(&serde_json::json!({ "body": body }))
        .send()
        .await
        .map_err(|error| format!("posting the comment failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub refused the comment: {}", response.status()));
    }
    #[derive(Deserialize)]
    struct Comment {
        id: i64,
    }
    let comment: Comment = response
        .json()
        .await
        .map_err(|error| format!("unexpected comment response: {error}"))?;
    sqlx::query(
        "INSERT INTO pr_comments (repo, pr_number, comment_id, session_id) VALUES (?, ?, ?, ?) \
         ON CONFLICT(repo, pr_number) DO UPDATE SET comment_id = excluded.comment_id, session_id = excluded.session_id",
    )
    .bind(&repo)
    .bind(event.number)
    .bind(comment.id)
    .bind(&session_id)
    .execute(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(if existing.is_some() { "updated" } else { "commented" }.to_string())
}

fn github_headers(request: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    request
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "i-blueprint-server")
}

async fn installation_token(app: &GithubApp, client: &reqwest::Client, installation_id: i64) -> Result<String, String> {
    #[derive(Deserialize)]
    struct Token {
        token: String,
    }
    let response = github_headers(
        client.post(format!("{}/app/installations/{installation_id}/access_tokens", app.api_base)),
        &app.app_jwt()?,
    )
    .send()
    .await
    .map_err(|error| format!("getting an installation token failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!("GitHub refused the installation token: {}", response.status()));
    }
    Ok(response
        .json::<Token>()
        .await
        .map_err(|error| format!("unexpected token response: {error}"))?
        .token)
}

#[derive(Debug, Deserialize)]
pub struct LinkRequest {
    /// `owner/name`, or empty/absent to unlink.
    #[serde(default)]
    pub repo: Option<String>,
}

/// `PUT /sessions/{id}/github`: links (or unlinks) the session's repo. Owner only.
pub async fn link_repo(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
    Json(request): Json<LinkRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_optional(&state.pool)
        .await?;
    match owner {
        None => return Err(AppError::NotFound("session not found".into())),
        Some((owner,)) if owner != user_id => {
            return Err(AppError::Unauthorized("only the session owner can link a repo".into()));
        }
        Some(_) => {}
    }
    let repo = request
        .repo
        .map(|repo| repo.trim().trim_end_matches(".git").trim_start_matches("https://github.com/").to_string())
        .filter(|repo| !repo.is_empty());
    if let Some(repo) = &repo
        && !is_repo_name(repo)
    {
        return Err(AppError::BadRequest("use the repo's owner/name, e.g. acme/web".into()));
    }
    sqlx::query("UPDATE sessions SET github_repo = ? WHERE id = ?")
        .bind(&repo)
        .bind(&session_id)
        .execute(&state.pool)
        .await?;
    state
        .hub
        .broadcast(&session_id, &serde_json::json!({"type": "github_repo", "repo": repo}))
        .await;
    Ok(Json(serde_json::json!({ "repo": repo })))
}

/// `GET /join/{id}`: a web page for the join link in PR comments and emails,
/// which hands off to the IDE (browsers and GitHub won't link to `i-ide://`).
pub async fn join_page(Path(session_id): Path<String>) -> Response {
    if session_id.is_empty() || !session_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    let link = format!("i-ide://i-mode/join/{session_id}");
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\">\
         <title>Join the I session</title>\
         <style>body{{font-family:system-ui,sans-serif;max-width:32rem;margin:4rem auto;padding:0 1rem;line-height:1.5}}\
         a.button{{display:inline-block;padding:.6rem 1.1rem;border-radius:8px;background:#111;color:#fff;text-decoration:none}}\
         code{{background:#eee;padding:.1rem .3rem;border-radius:4px}}</style></head><body>\
         <h1>Join the session in I</h1>\
         <p><a class=\"button\" href=\"{link}\">Open in I</a></p>\
         <p>Nothing happened? Make sure I is installed, then paste this into its I-mode panel's Join form: <code>{session_id}</code></p>\
         </body></html>"
    ))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let bytes = mac.finalize().into_bytes();
        format!("sha256={}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    #[test]
    fn signatures_are_checked() {
        let body = br#"{"zen":"hi"}"#;
        assert!(signature_is_valid("s3cret", body, &sign("s3cret", body)));
        assert!(!signature_is_valid("other", body, &sign("s3cret", body)));
        assert!(!signature_is_valid("s3cret", b"tampered", &sign("s3cret", body)));
        assert!(!signature_is_valid("s3cret", body, "sha1=abc"));
        assert!(!signature_is_valid("s3cret", body, "sha256=zz"));
    }

    #[test]
    fn session_links_and_repo_names_are_recognised() {
        assert_eq!(
            session_id_in("Fixes #3\n\nSession: https://x.test/join/3170e702-3ec9-4ff1-a810-3d495a755c41 thanks").as_deref(),
            Some("3170e702-3ec9-4ff1-a810-3d495a755c41")
        );
        assert_eq!(
            session_id_in("i-ide://i-mode/join/3170e702-3ec9").as_deref(),
            Some("3170e702-3ec9")
        );
        assert_eq!(session_id_in("no link here"), None);
        assert!(is_repo_name("acme/web-app"));
        assert!(is_repo_name("Info-stats-ai/I-ide"));
        assert!(!is_repo_name("acme"));
        assert!(!is_repo_name("acme/web/extra"));
        assert!(!is_repo_name("acme/<script>"));
    }

    #[test]
    fn the_comment_reads_well_and_links_only_when_given_a_url() {
        let totals = SessionTotals {
            name: "Tiny Tasks".into(),
            conflicts_caught: 3,
            agent_collisions: 1,
            divergences: 2,
            lane_warnings: 0,
            live_merges: 120,
            people: 2,
        };
        let private = comment_body(&totals, Some("https://x.test/join/abc"));
        assert!(private.starts_with("**Built in an I session: Tiny Tasks** (2 people)"));
        assert!(private.contains("| Conflicts caught before they landed | **3** |"));
        assert!(private.contains("[Open the session in I](https://x.test/join/abc)"));
        let public = comment_body(&totals, None);
        assert!(!public.contains("/join/"));
    }

    /// A stand-in for api.github.com that records what it was sent.
    async fn mock_github() -> (String, Arc<Mutex<Vec<(String, String, String)>>>) {
        use axum::extract::Request;
        let seen: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let recorder = seen.clone();
        let app = axum::Router::new().fallback(move |request: Request| {
            let recorder = recorder.clone();
            async move {
                let method = request.method().to_string();
                let path = request.uri().path().to_string();
                let auth = request
                    .headers()
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                let body = axum::body::to_bytes(request.into_body(), 1 << 20).await.unwrap();
                recorder.lock().unwrap().push((method.clone(), path.clone(), String::from_utf8_lossy(&body).into()));
                if path.ends_with("/access_tokens") {
                    assert!(auth.starts_with("Bearer ey"), "the app authenticates with a JWT");
                    Json(serde_json::json!({"token": "inst-token"})).into_response()
                } else {
                    assert_eq!(auth, "Bearer inst-token", "comments use the installation token");
                    Json(serde_json::json!({"id": 777})).into_response()
                }
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), seen)
    }

    #[tokio::test]
    async fn a_pull_request_gets_one_comment_that_later_pushes_update() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        for statement in [
            "INSERT INTO users (id, email, password_hash, created_at) VALUES ('u1', 'a@example.test', 'x', '2026-01-01T00:00:00Z')",
            "INSERT INTO sessions (id, name, owner_id, created_at, github_repo) VALUES ('3170e702-aaaa', 'Tiny Tasks', 'u1', '2026-01-01T00:00:00Z', 'acme/web')",
            "INSERT INTO sessions (id, name, owner_id, created_at, github_repo) VALUES ('9999e702-bbbb', 'Someone else', 'u1', '2026-01-02T00:00:00Z', 'other/repo')",
            "INSERT INTO session_members (session_id, user_id, role, joined_at) VALUES ('3170e702-aaaa', 'u1', 'owner', '2026-01-01T00:00:00Z')",
            "INSERT INTO conflict_daily (session_id, day, kind, count) VALUES ('3170e702-aaaa', '2026-01-01', 'agent_collision', 2)",
            "INSERT INTO conflict_daily (session_id, day, kind, count) VALUES ('3170e702-aaaa', '2026-01-01', 'merge', 50)",
        ] {
            sqlx::query(statement).execute(&pool).await.unwrap();
        }
        let (api_base, seen) = mock_github().await;
        // A throwaway key made for this run, so no private key lives in the repo.
        let key_path = std::env::temp_dir().join(format!("github-app-test-{}.pem", std::process::id()));
        let generated = std::process::Command::new("openssl")
            .args(["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out"])
            .arg(&key_path)
            .output()
            .is_ok_and(|output| output.status.success());
        if !generated {
            eprintln!("skipping: openssl isn't available to make a test key");
            return;
        }
        let pem = std::fs::read_to_string(&key_path).unwrap();
        let _ = std::fs::remove_file(&key_path);
        let app = GithubApp::for_test(&pem, "s3cret", api_base);
        let state = crate::test_state(pool.clone());

        let event = |action: &str, body: &str| {
            serde_json::from_value::<PullRequestEvent>(serde_json::json!({
                "action": action,
                "number": 42,
                "pull_request": {"body": body},
                "repository": {"full_name": "acme/web", "private": true},
                "installation": {"id": 9}
            }))
            .unwrap()
        };
        // Naming a session linked to another repo doesn't leak it: this
        // repo's own session is used.
        let outcome = handle_pull_request(&state, &app, event("opened", "see /join/9999e702-bbbb")).await.unwrap();
        assert_eq!(outcome, "commented");
        let outcome = handle_pull_request(&state, &app, event("synchronize", "")).await.unwrap();
        assert_eq!(outcome, "updated");
        assert_eq!(handle_pull_request(&state, &app, event("closed", "")).await.unwrap(), "ignored");

        let seen = seen.lock().unwrap();
        let calls: Vec<(String, String)> = seen.iter().map(|(method, path, _)| (method.clone(), path.clone())).collect();
        assert_eq!(
            calls,
            vec![
                ("POST".into(), "/app/installations/9/access_tokens".into()),
                ("POST".into(), "/repos/acme/web/issues/42/comments".into()),
                ("POST".into(), "/app/installations/9/access_tokens".into()),
                ("PATCH".into(), "/repos/acme/web/issues/comments/777".into()),
            ]
        );
        let comment = &seen[1].2;
        assert!(comment.contains("Tiny Tasks"), "{comment}");
        assert!(!comment.contains("Someone else"));
        assert!(comment.contains("https://blueprint.example.test/join/3170e702-aaaa"));
    }
}
