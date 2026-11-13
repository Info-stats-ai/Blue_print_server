mod agent;
mod auth;
mod checkpoint;
mod client_ip;
mod crdt;
mod db;
mod email;
mod errors;
mod extensions;
mod github;
mod guard;
mod handlers;
mod history;
mod listen;
mod memory;
mod models;
mod oauth;
mod preview;
mod site;
mod stats;
mod symbols;
mod team;
mod ws;

use axum::routing::{any, get, post, put};
use axum::{Router, middleware};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub hub: ws::Hub,
    pub jwt_secret: String,
    pub oauth: oauth::OAuthState,
    pub exchange: oauth::ExchangeState,
    // Shared across every outbound call this server makes (Anthropic,
    // GitHub, Google) instead of each call site building its own
    // `reqwest::Client`. A `Client` owns a connection pool; constructing a
    // fresh one per call throws away keep-alive/TLS-session reuse and pays
    // a new handshake every single time — at real request volume (every
    // watcher/blueprint LLM call, every OAuth login) that's a meaningful,
    // avoidable latency and CPU cost. `Client` is cheap to clone (an `Arc`
    // internally), so sharing it this way is the documented pattern.
    pub http_client: reqwest::Client,
    pub previews: preview::Previews,
    pub preview_routing: preview::Routing,
    // Host that serves shared apps, kept apart from this server's own pages.
    pub preview_host: Option<String>,
    // Set when the GitHub App's credentials are configured.
    pub github: Option<std::sync::Arc<github::GithubApp>>,
    pub listen_limits: listen::Limits,
}

#[cfg(test)]
pub(crate) fn test_state(pool: SqlitePool) -> AppState {
    AppState {
        pool,
        hub: ws::Hub::new(),
        jwt_secret: "test".to_string(),
        oauth: oauth::OAuthState::new(),
        exchange: oauth::ExchangeState::new(),
        http_client: reqwest::Client::new(),
        previews: preview::Previews::default(),
        preview_routing: preview::Routing::default(),
        preview_host: None,
        github: None,
        listen_limits: listen::Limits::default(),
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let database_url = db::resolve_database_url(
        std::env::var("DATABASE_URL").ok(),
        std::env::var("RAILWAY_VOLUME_MOUNT_PATH").ok(),
    );
    println!("database: {database_url}");
    println!("anthropic key: {}", agent::describe_anthropic_key());
    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");

    let pool = db::init_pool(&database_url)
        .await
        .expect("failed to initialize database");

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build shared http client");

    let state = AppState {
        pool,
        hub: ws::Hub::new(),
        jwt_secret,
        oauth: oauth::OAuthState::new(),
        exchange: oauth::ExchangeState::new(),
        http_client,
        previews: preview::Previews::default(),
        preview_routing: preview::Routing::default(),
        preview_host: std::env::var("PREVIEW_HOST").ok().filter(|host| !host.trim().is_empty()),
        github: github::GithubApp::from_env(),
        listen_limits: listen::Limits::default(),
    };
    println!(
        "github app: {}",
        if state.github.is_some() { "configured" } else { "not configured" }
    );

    state.hub.set_event_sink(stats::spawn_writer(state.pool.clone()));
    state.hub.ai_budget.restore(&state.pool).await;
    if let Ok(pruned) = history::prune_blobs(&state.pool).await
        && pruned > 0
    {
        println!("history: removed {pruned} unused file version(s)");
    }

    let bundled = extensions::load_bundled(&state.pool, std::env::var("EXTENSIONS_DIR").ok().as_deref()).await;
    if bundled > 0 {
        println!("extensions: loaded {bundled} bundled package(s)");
    }

    match checkpoint::prune_checkpoints(&state.pool, None).await {
        Ok(0) => {}
        Ok(pruned) => println!("checkpoint: pruned {pruned} old checkpoint(s)"),
        Err(error) => eprintln!("checkpoint: pruning old checkpoints failed: {error}"),
    }
    tokio::spawn(checkpoint::run_checkpoint_sweep(state.clone()));

    // Token bucket: refills 1 req/600ms (~100/min sustained) per peer IP, burst up to 100.
    let client_ip = client_ip::ClientIp::from_env();
    let global_governor_conf = GovernorConfigBuilder::default()
        .key_extractor(client_ip)
        .period(Duration::from_millis(600))
        .burst_size(100)
        .finish()
        .expect("valid global rate limit config");

    // Token bucket: refills 1 req/6s (~10/min sustained) per peer IP, burst up to 10.
    let auth_governor_conf = GovernorConfigBuilder::default()
        .key_extractor(client_ip)
        .period(Duration::from_millis(6000))
        .burst_size(10)
        .finish()
        .expect("valid auth rate limit config");

    // The governor crate's per-IP storage only grows; purge stale entries periodically.
    let global_limiter = global_governor_conf.limiter().clone();
    let auth_limiter = auth_governor_conf.limiter().clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            global_limiter.retain_recent();
            auth_limiter.retain_recent();
        }
    });

    let auth_routes = Router::new()
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        // Phase 8 desktop-loopback OAuth. Same rate-limit tier as
        // register/login — it's still the sign-in surface, just reached via
        // a browser redirect instead of a direct API call.
        .route("/auth/{provider}", get(oauth::start))
        .route("/auth/{provider}/callback", get(oauth::callback))
        .route("/auth/exchange", post(oauth::exchange))
        .route("/waitlist", post(site::join_waitlist))
        .layer(GovernorLayer::new(auth_governor_conf));

    let public_routes = Router::new()
        .route("/ws", get(ws::ws_handler))
        .route("/github/webhook", post(github::webhook))
        .route("/join/{id}", get(github::join_page))
        .route("/auth/signin", get(site::signin))
        // The extension store, in the shape the IDE's extension host expects.
        .route("/api/extensions", get(extensions::search))
        .route("/api/extensions/updates", get(extensions::updates))
        .route(
            "/api/extensions/{id}",
            get(extensions::versions)
                .post(extensions::publish)
                .layer(axum::extract::DefaultBodyLimit::max(extensions::MAX_ARCHIVE_BYTES + 1024)),
        )
        .route("/api/extensions/{id}/download", get(extensions::download_latest))
        .route("/api/extensions/{id}/{version}/download", get(extensions::download_version));

    let protected_routes = Router::new()
        .route(
            "/sessions",
            post(handlers::create_session).get(handlers::list_sessions),
        )
        .route("/sessions/{id}/join", post(handlers::join_session))
        .route("/sessions/{id}/invite", post(handlers::invite_to_session))
        .route("/sessions/{id}/join-requests/{user_id}", put(handlers::answer_join_request))
        .route("/sessions/{id}/memory", get(memory::search_handler))
        .route("/sessions/{id}/history", get(history::list_handler))
        .route("/sessions/{id}/history/{version}", get(history::version_handler))
        .route(
            "/contracts",
            post(handlers::create_contract).get(handlers::list_contracts),
        )
        .route("/contracts/{id}", put(handlers::update_contract))
        .route("/intent", post(handlers::create_intent))
        .route("/intent/{id}", put(handlers::update_intent))
        .route("/intents", get(handlers::list_intents))
        .route("/metrics/weekly", get(stats::weekly))
        .route("/sessions/{id}/github", put(github::link_repo))
        .route(
            "/plans",
            post(handlers::create_plan).get(handlers::list_plans),
        )
        .route("/plans/{id}/approve", put(handlers::approve_plan))
        .route("/plans/{id}/reject", put(handlers::reject_plan))
        .route("/plans/{id}/restore", put(handlers::restore_plan))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::auth_middleware,
        ));

    let app = Router::new()
        .merge(auth_routes)
        .merge(public_routes)
        .merge(protected_routes)
        .layer(GovernorLayer::new(global_governor_conf))
        // Added after the layers so they're outside the per-IP rate limit:
        // one page load of a dev app makes dozens of asset requests.
        .route(preview::EVENTS_PATH, get(preview::events))
        .route("/preview/{token}", any(preview::open))
        .route("/preview/{token}/", any(preview::open))
        .route("/preview/{token}/{*rest}", any(preview::open))
        .fallback(preview::fallback)
        .layer(middleware::from_fn_with_state(state.clone(), preview::separate_hosts))
        .with_state(state);

    // Railway assigns a dynamic port via $PORT and routes traffic to it, not to a
    // fixed port — binding to a hardcoded port here caused "Application failed to
    // respond" since Railway's proxy couldn't reach the process.
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let bind_addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|error| panic!("failed to bind to {bind_addr}: {error}"));

    println!("blueprint-server listening on {bind_addr}");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("server error");
}
