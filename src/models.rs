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
    // Whether owner_id has final authority (sharing scope, reference
    // approval, triage) over this session, or everyone's equal. See the
    // migration that added this column for why it defaults to true.
    pub has_leader: bool,
    // What this session is building — the north-star sentence(s) the creator
    // writes at session start. Blank for sessions created before this column
    // was added. Fed into @agent blueprint's system prompt so the AI knows
    // what "relevant" means when summarising the codebase.
    pub goal: String,
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
    // "reference" (a proposed reference.md, approving it publishes
    // `content` as the live blueprint) or "triage" (a multi-file review
    // report — see `agent::run_triage_job` — whose approval just
    // acknowledges it; see `handlers::approve_plan`). Any plan from before
    // this column existed reads back as "reference", matching what it
    // always implicitly was.
    pub kind: String,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChatMessage {
    // The `chat_messages` row id: a stable, increasing cursor clients page
    // back through history with. 0 only for messages built in tests.
    #[serde(default)]
    pub id: i64,
    pub sender: String,
    pub text: String,
    // Who asked, on an AI reply relayed from a client, so that client can
    // skip the echo of a reply it already shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

// ---- Sessions ----

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    pub name: String,
    // Defaults to true (leader-led, matching every session created before
    // this existed) when the client omits it — see Session::has_leader.
    #[serde(default = "default_true")]
    pub has_leader: bool,
    // Optional: blank when the creator skips it. Stored server-side and
    // included in every Snapshot so all peers and @agent blueprint see it.
    #[serde(default)]
    pub goal: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize)]
pub struct SessionResponse {
    pub session: Session,
}

#[derive(Debug, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct InviteRequest {
    pub email: String,
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
