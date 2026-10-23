use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct SessionMember {
    pub session_id: String,
    pub user_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Contract {
    pub id: String,
    pub session_id: String,
    pub owner_id: String,
    pub name: String,
    pub content: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Intent {
    pub id: String,
    pub session_id: String,
    pub user_id: String,
    pub description: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ExecutionPlan {
    pub id: String,
    pub session_id: String,
    pub author_id: String,
    pub content: String,
    pub status: String,
    pub reviewed_by: Option<String>,
    pub review_note: Option<String>,
    // JSON-encoded {path: content} snapshot of every synced file in the
    // session at the moment this plan was created — what `restore_plan`
    // writes back. `None` for plans created before this existed.
    pub files: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---- Auth ----

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user_id: String,
}

// ---- Chat ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub sender: String,
    pub text: String,
}

// ---- Sessions ----

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session: Session,
}

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

// ---- Contracts ----

#[derive(Debug, Deserialize)]
pub struct CreateContractRequest {
    pub session_id: String,
    pub name: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContractRequest {
    pub name: Option<String>,
    pub content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct ContractResponse {
    pub contract: Contract,
}

// ---- Intents ----

#[derive(Debug, Deserialize)]
pub struct CreateIntentRequest {
    pub session_id: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIntentRequest {
    pub description: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IntentResponse {
    pub intent: Intent,
}

// ---- Execution Plans ----

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub session_id: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct RejectPlanRequest {
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PlanResponse {
    pub plan: ExecutionPlan,
    pub status: String,
}
