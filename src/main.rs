mod agent;
mod auth;
mod checkpoint;
mod crdt;
mod db;
mod errors;
mod handlers;
mod models;
mod symbols;
mod ws;

use axum::routing::{get, post, put};
use axum::{Router, middleware};
use sqlx::SqlitePool;
use std::net::SocketAddr;
use std::time::Duration;
use tower_governor::{GovernorLayer, governor::GovernorConfigBuilder};
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub hub: ws::Hub,
    pub jwt_secret: String,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    // Railway sets DATABASE_URL explicitly to a volume-mounted path; this
    // default just keeps local/ad-hoc startup from panicking when it's unset.
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:///data/blueprint.db".to_string());
    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");

    let pool = db::init_pool(&database_url)
        .await
        .expect("failed to initialize database");

    let state = AppState {
        pool,
        hub: ws::Hub::new(),
        jwt_secret,
    };

    tokio::spawn(checkpoint::run_checkpoint_sweep(state.clone()));

    // Token bucket: refills 1 req/600ms (~100/min sustained) per peer IP, burst up to 100.
    let global_governor_conf = GovernorConfigBuilder::default()
        .period(Duration::from_millis(600))
        .burst_size(100)
        .finish()
        .expect("valid global rate limit config");

    // Token bucket: refills 1 req/6s (~10/min sustained) per peer IP, burst up to 10.
    let auth_governor_conf = GovernorConfigBuilder::default()
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
        .layer(GovernorLayer::new(auth_governor_conf));

    let public_routes = Router::new().route("/ws", get(ws::ws_handler));

    let protected_routes = Router::new()
        .route(
            "/sessions",
            post(handlers::create_session).get(handlers::list_sessions),
        )
        .route("/sessions/{id}/join", post(handlers::join_session))
        .route(
            "/contracts",
            post(handlers::create_contract).get(handlers::list_contracts),
        )
        .route("/contracts/{id}", put(handlers::update_contract))
        .route("/intent", post(handlers::create_intent))
        .route("/intent/{id}", put(handlers::update_intent))
        .route("/intents", get(handlers::list_intents))
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
        .layer(CorsLayer::permissive())
        .layer(GovernorLayer::new(global_governor_conf))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to port 3000");

    println!("blueprint-server listening on 0.0.0.0:3000");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .expect("server error");
}
