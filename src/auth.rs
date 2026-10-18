use axum::{
    extract::{Request, State},
    http::header::AUTHORIZATION,
    middleware::Next,
    response::Response,
    Json,
};
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::{AuthResponse, LoginRequest, RegisterRequest, User};
use crate::AppState;

const TOKEN_TTL_SECONDS: i64 = 60 * 60 * 24 * 7; // 7 days

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
}

#[derive(Debug, Clone)]
pub struct AuthUser(pub String);

pub fn create_jwt(user_id: &str, secret: &str) -> Result<String, AppError> {
    let exp = (Utc::now().timestamp() + TOKEN_TTL_SECONDS) as usize;
    let claims = Claims {
        sub: user_id.to_string(),
        exp,
    };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(AppError::from)
}

pub fn decode_jwt(token: &str, secret: &str) -> Result<Claims, AppError> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )?;
    Ok(data.claims)
}

async fn hash_password(password: String) -> Result<String, AppError> {
    tokio::task::spawn_blocking(move || hash(password, DEFAULT_COST))
        .await
        .map_err(|e| AppError::Internal(format!("hashing task panicked: {e}")))?
        .map_err(AppError::from)
}

async fn verify_password(password: String, password_hash: String) -> Result<bool, AppError> {
    tokio::task::spawn_blocking(move || verify(password, &password_hash))
        .await
        .map_err(|e| AppError::Internal(format!("verify task panicked: {e}")))?
        .map_err(AppError::from)
}

pub async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    if payload.email.trim().is_empty() || payload.password.is_empty() {
        return Err(AppError::BadRequest("email and password are required".into()));
    }

    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM users WHERE email = ?")
        .bind(&payload.email)
        .fetch_optional(&state.pool)
        .await?;
    if existing.is_some() {
        return Err(AppError::Conflict("email already registered".into()));
    }

    let password_hash = hash_password(payload.password.clone()).await?;
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO users (id, email, password_hash, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&payload.email)
    .bind(&password_hash)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let token = create_jwt(&id, &state.jwt_secret)?;
    Ok(Json(AuthResponse { token, user_id: id }))
}

pub async fn login(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AppError> {
    let user: User = sqlx::query_as("SELECT * FROM users WHERE email = ?")
        .bind(&payload.email)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::Unauthorized("invalid email or password".into()))?;

    let valid = verify_password(payload.password.clone(), user.password_hash.clone()).await?;
    if !valid {
        return Err(AppError::Unauthorized("invalid email or password".into()));
    }

    let token = create_jwt(&user.id, &state.jwt_secret)?;
    Ok(Json(AuthResponse {
        token,
        user_id: user.id,
    }))
}

pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let header_value = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::Unauthorized("missing authorization header".into()))?;

    let token = header_value
        .strip_prefix("Bearer ")
        .ok_or_else(|| AppError::Unauthorized("invalid authorization header".into()))?;

    let claims = decode_jwt(token, &state.jwt_secret)?;

    req.extensions_mut().insert(AuthUser(claims.sub));

    Ok(next.run(req).await)
}
