//! The public website: home, download, docs and the sign-in page the IDE opens.
//!
//! Pages are compiled in, so a deploy can't lose them. Home, download and docs
//! are reached through `preview::fallback`, so they never shadow a path of an
//! app someone is sharing. The sign-in page lives under `/auth`, which is
//! already this server's own.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
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

const DOC_PAGES: &[(&str, &str)] = &[
    ("getting-started", include_str!("../site/docs/getting-started.html")),
    ("installation", include_str!("../site/docs/installation.html")),
    ("interface", include_str!("../site/docs/interface.html")),
    ("tutorial", include_str!("../site/docs/tutorial.html")),
    ("editing", include_str!("../site/docs/editing.html")),
    ("navigation", include_str!("../site/docs/navigation.html")),
    ("terminal-tasks", include_str!("../site/docs/terminal-tasks.html")),
    ("extensions", include_str!("../site/docs/extensions.html")),
    ("git", include_str!("../site/docs/git.html")),
    ("settings", include_str!("../site/docs/settings.html")),
    ("i-mode", include_str!("../site/docs/i-mode.html")),
    ("sign-in", include_str!("../site/docs/sign-in.html")),
    ("sessions", include_str!("../site/docs/sessions.html")),
    ("invites", include_str!("../site/docs/invites.html")),
    ("chat", include_str!("../site/docs/chat.html")),
    ("live-editing", include_str!("../site/docs/live-editing.html")),
    ("tasks", include_str!("../site/docs/tasks.html")),
    ("team-tab", include_str!("../site/docs/team-tab.html")),
    ("previews", include_str!("../site/docs/previews.html")),
    ("ai-setup", include_str!("../site/docs/ai-setup.html")),
    ("your-ai", include_str!("../site/docs/your-ai.html")),
    ("agent-panel", include_str!("../site/docs/agent-panel.html")),
    ("claims", include_str!("../site/docs/claims.html")),
    ("team-agent", include_str!("../site/docs/team-agent.html")),
    ("agent-runs", include_str!("../site/docs/agent-runs.html")),
    ("reference-doc", include_str!("../site/docs/reference-doc.html")),
    ("triage", include_str!("../site/docs/triage.html")),
    ("settings-reference", include_str!("../site/docs/settings-reference.html")),
    ("controls", include_str!("../site/docs/controls.html")),
    ("limits", include_str!("../site/docs/limits.html")),
    ("security", include_str!("../site/docs/security.html")),
    ("troubleshooting", include_str!("../site/docs/troubleshooting.html")),
    ("faq", include_str!("../site/docs/faq.html")),
];

const DOC_OVERVIEW: &str = include_str!("../site/docs/overview.html");

const DOC_IMAGES: &[(&str, &[u8])] = &[
    ("window.jpg", include_bytes!("../site/docs-img/window.jpg")),
    ("people-menu.jpg", include_bytes!("../site/docs-img/people-menu.jpg")),
    ("join-request.jpg", include_bytes!("../site/docs-img/join-request.jpg")),
    ("team-chat.jpg", include_bytes!("../site/docs-img/team-chat.jpg")),
    ("team-activity.jpg", include_bytes!("../site/docs-img/team-activity.jpg")),
    ("plan-mode.jpg", include_bytes!("../site/docs-img/plan-mode.jpg")),
    ("signed-out.jpg", include_bytes!("../site/docs-img/signed-out.jpg")),
];

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; \
    style-src 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
    script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; media-src 'self'; \
    base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Where search engines should send people, whichever address a page was
/// reached on.
const PUBLIC_SITE: &str = "https://i-ide.dev";

fn indexable(template: &str, path: &str) -> Response {
    let canonical = format!("<link rel=\"canonical\" href=\"{PUBLIC_SITE}{path}\">\n</head>");
    html(&template.replacen("</head>", &canonical, 1))
}

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
        "" => indexable(HOME, "/"),
        "/download" => indexable(DOWNLOAD, "/download"),
        "/robots.txt" => plain(format!("User-agent: *\nAllow: /\nSitemap: {PUBLIC_SITE}/sitemap.xml\n"), "text/plain"),
        "/sitemap.xml" => plain(sitemap(), "application/xml"),
        "/docs" => doc_page(DOC_OVERVIEW),
        "/docs/search.json" => doc_search_index(),
        path if path.strip_prefix("/docs/").is_some_and(|slug| doc_article(slug).is_some()) => {
            doc_page(doc_article(&path["/docs/".len()..]).unwrap_or(DOC_OVERVIEW))
        }
        path if path.starts_with("/docs/img/") => doc_image(&path["/docs/img/".len()..]),
        path if path.starts_with("/docs/") || path.starts_with("/releases") || path.starts_with("/channel") => {
            Redirect::permanent(legacy_doc_target(path)).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "Not found").into_response(),
    }
}

/// The app inherited links to Zed's docs layout, such as `/docs/linux`.
/// Each goes to the closest page here instead of a dead end.
fn legacy_doc_target(path: &str) -> &'static str {
    const TARGETS: &[(&str, &str)] = &[
        ("/docs/linux", "/docs/installation"),
        ("/docs/windows", "/docs/installation"),
        ("/docs/macos", "/docs/installation"),
        ("/docs/getting-started", "/docs/getting-started"),
        ("/docs/configuring-languages", "/docs/editing"),
        ("/docs/languages", "/docs/editing"),
        ("/docs/completions", "/docs/editing"),
        ("/docs/editor", "/docs/editing"),
        ("/docs/multibuffers", "/docs/editing"),
        ("/docs/markdown", "/docs/editing"),
        ("/docs/key-bindings", "/docs/settings"),
        ("/docs/settings", "/docs/settings"),
        ("/docs/themes", "/docs/settings"),
        ("/docs/icon-themes", "/docs/settings"),
        ("/docs/vim", "/docs/settings"),
        ("/docs/telemetry", "/docs/settings-reference"),
        ("/docs/tasks", "/docs/terminal-tasks"),
        ("/docs/debugger", "/docs/terminal-tasks"),
        ("/docs/repl", "/docs/terminal-tasks"),
        ("/docs/git", "/docs/git"),
        ("/docs/ai", "/docs/ai-setup"),
        ("/docs/remote-development", "/docs/faq"),
        ("/releases", "/download"),
        ("/channel", "/"),
    ];
    TARGETS
        .iter()
        .find(|(prefix, _)| path == *prefix || path.starts_with(&format!("{prefix}/")) || path.starts_with(&format!("{prefix}.")))
        .map_or("/docs", |(_, target)| target)
}

fn doc_article(slug: &str) -> Option<&'static str> {
    DOC_PAGES.iter().find(|(page, _)| *page == slug).map(|(_, article)| *article)
}

/// A value from an article's opening tag, such as `data-title`.
fn article_attribute<'a>(article: &'a str, name: &str) -> &'a str {
    article
        .split_once(&format!("{name}=\""))
        .and_then(|(_, rest)| rest.split_once('"'))
        .map_or("", |(value, _)| value)
}

/// Every page in reading order: the overview, then the index's order.
fn doc_articles() -> impl Iterator<Item = &'static str> {
    std::iter::once(DOC_OVERVIEW).chain(DOC_PAGES.iter().map(|(_, article)| *article))
}

fn pager_link(article: Option<&str>, label: &str) -> String {
    article.map_or_else(
        || "<span></span>".to_string(),
        |article| {
            format!(
                "<a href=\"{}\"><small>{label}</small>{}</a>",
                article_attribute(article, "data-page"),
                article_attribute(article, "data-title"),
            )
        },
    )
}

/// One docs page: the shared index, with this page marked and its group open.
fn doc_page(article: &str) -> Response {
    let path = article_attribute(article, "data-page");
    let articles: Vec<&str> = doc_articles().collect();
    let position = articles.iter().position(|other| *other == article).unwrap_or(0);
    let pager = format!(
        "<nav class=\"pager\" aria-label=\"Previous and next pages\">{}{}</nav>",
        pager_link(position.checked_sub(1).map(|previous| articles[previous]), "Previous"),
        pager_link(articles.get(position + 1).copied(), "Next"),
    );
    let mut page = DOCS
        .replace("{{TITLE}}", article_attribute(article, "data-title"))
        .replace("{{ARTICLE}}", article)
        .replace("{{PAGER}}", &pager);
    let link = format!("<a href=\"{path}\"");
    if let Some(at) = page.find(&link) {
        page.insert_str(at + "<a ".len(), "aria-current=\"page\" ");
        if let Some(group) = page[..at].rfind("<details>") {
            page.insert_str(group + "<details".len(), " open");
        }
    }
    indexable(&page, path)
}

fn plain(body: String, content_type: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

fn sitemap() -> String {
    let urls: String = ["/", "/download"]
        .into_iter()
        .chain(doc_articles().map(|article| article_attribute(article, "data-page")))
        .map(|path| format!("<url><loc>{PUBLIC_SITE}{path}</loc></url>"))
        .collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{urls}</urlset>")
}

fn strip_tags(fragment: &str) -> String {
    let mut text = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for character in fragment.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn doc_search_index() -> Response {
    let pages: Vec<_> = doc_articles()
        .map(|article| {
            serde_json::json!({
                "path": article_attribute(article, "data-page"),
                "title": article_attribute(article, "data-title"),
                "group": article_attribute(article, "data-group"),
                "text": strip_tags(article),
            })
        })
        .collect();
    Json(pages).into_response()
}

fn doc_image(name: &str) -> Response {
    match DOC_IMAGES.iter().find(|(image, _)| *image == name) {
        Some((_, bytes)) => (
            [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "public, max-age=3600")],
            *bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "Not found").into_response(),
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
        for (slug, article) in DOC_PAGES {
            assert!(article.contains(&format!("data-page=\"/docs/{slug}\"")), "{slug}");
            assert_eq!(page(&format!("/docs/{slug}")).status(), StatusCode::OK);
        }
        // Unknown docs paths, such as old links from the app, go to the docs home.
        assert_eq!(page("/docs/nope").headers()[header::LOCATION], "/docs");
    }

    async fn body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[test]
    fn docs_links_and_images_all_resolve() {
        for article in doc_articles() {
            for link in article.split("href=\"/docs/").skip(1) {
                let target = link.split(['"', '#']).next().unwrap();
                assert_eq!(page(&format!("/docs/{target}")).status(), StatusCode::OK, "link to {target}");
            }
            for image in article.split("src=\"/docs/img/").skip(1) {
                let name = image.split('"').next().unwrap();
                assert_eq!(doc_image(name).status(), StatusCode::OK, "image {name}");
            }
        }
    }

    #[tokio::test]
    async fn each_docs_page_holds_only_its_own_content() {
        let claims = body(page("/docs/claims")).await;
        assert_eq!(claims.matches("<article").count(), 1);
        assert!(claims.contains("<title>Function claims | I-IDE Docs</title>"));
        assert!(claims.contains("<a aria-current=\"page\" href=\"/docs/claims\""));
        assert!(claims.contains("<details open><summary>Work with AI"));
        assert_eq!(claims.matches("<details open>").count(), 1);
        assert!(!claims.contains("Tutorial: your first team session</h1>"));
        assert!(claims.contains("<small>Previous</small>"));
        assert!(claims.contains("<small>Next</small>"));

        let overview = body(page("/docs")).await;
        assert_eq!(overview.matches("<article").count(), 1);
        assert!(overview.contains("<link rel=\"canonical\" href=\"https://i-ide.dev/docs\">"));
        assert!(!overview.contains("<small>Previous</small>"));
    }

    #[test]
    fn old_zed_docs_links_land_on_a_real_page() {
        for (old, new) in [
            ("/docs/linux", "/docs/installation"),
            ("/docs/languages/python", "/docs/editing"),
            ("/docs/ai/external-agents", "/docs/ai-setup"),
            ("/docs/getting-started.html", "/docs/getting-started"),
            ("/docs/nightly/settings", "/docs"),
            ("/releases", "/download"),
        ] {
            let response = page(old);
            assert_eq!(response.status(), StatusCode::PERMANENT_REDIRECT, "{old}");
            assert_eq!(response.headers()[header::LOCATION], new, "{old}");
        }
        assert_eq!(page("/docs/claims").status(), StatusCode::OK);
        assert_eq!(page("/nope").status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_sitemap_lists_every_page() {
        let sitemap = body(page("/sitemap.xml")).await;
        assert!(sitemap.contains("<loc>https://i-ide.dev/docs/tutorial</loc>"));
        assert!(body(page("/robots.txt")).await.contains("Sitemap: https://i-ide.dev/sitemap.xml"));
    }

    #[tokio::test]
    async fn search_covers_every_page() {
        let index: serde_json::Value = serde_json::from_str(&body(page("/docs/search.json")).await).unwrap();
        assert_eq!(index.as_array().unwrap().len(), DOC_PAGES.len() + 1);
        assert!(index.to_string().contains("Collision avoided"));
        assert!(!index.to_string().contains("<strong>"));
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
