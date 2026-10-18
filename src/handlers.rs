use axum::extract::{Extension, Path, Query, State};
use axum::Json;
use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::errors::AppError;
use crate::models::{
    Contract, ContractResponse, CreatePlanRequest, CreateContractRequest, CreateIntentRequest,
    CreateSessionRequest, ExecutionPlan, Intent, IntentResponse, ListQuery, OkResponse,
    PlanResponse, RejectPlanRequest, Session, SessionResponse, UpdateContractRequest,
    UpdateIntentRequest,
};
use crate::AppState;

async fn require_member(
    state: &AppState,
    session_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    let membership: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM session_members WHERE session_id = ? AND user_id = ?",
    )
    .bind(session_id)
    .bind(user_id)
    .fetch_optional(&state.pool)
    .await?;

    membership
        .map(|_| ())
        .ok_or_else(|| AppError::Unauthorized("not a member of this session".into()))
}

async fn require_session_owner(
    state: &AppState,
    session_id: &str,
    user_id: &str,
) -> Result<(), AppError> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT owner_id FROM sessions WHERE id = ?")
        .bind(session_id)
        .fetch_optional(&state.pool)
        .await?;

    match owner {
        Some((owner_id,)) if owner_id == user_id => Ok(()),
        Some(_) => Err(AppError::Unauthorized("only the session owner can do this".into())),
        None => Err(AppError::NotFound("session not found".into())),
    }
}

async fn broadcast_delta(
    state: &AppState,
    event_type: &str,
    session_id: &str,
    author_id: &str,
    data: serde_json::Value,
) {
    state
        .hub
        .broadcast(
            session_id,
            &json!({
                "type": event_type,
                "data": data,
                "author_id": author_id,
                "session_id": session_id,
                "timestamp": Utc::now(),
            }),
        )
        .await;
}

// ---- Sessions ----

pub async fn create_session(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(payload): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    if payload.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query("INSERT INTO sessions (id, name, owner_id, created_at) VALUES (?, ?, ?, ?)")
        .bind(&id)
        .bind(&payload.name)
        .bind(&user_id)
        .bind(now)
        .execute(&state.pool)
        .await?;

    sqlx::query(
        "INSERT INTO session_members (session_id, user_id, role, joined_at) VALUES (?, ?, 'owner', ?)",
    )
    .bind(&id)
    .bind(&user_id)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let session = Session {
        id,
        name: payload.name,
        owner_id: user_id,
        created_at: now,
    };

    Ok(Json(SessionResponse { session }))
}

pub async fn list_sessions(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
) -> Result<Json<Vec<Session>>, AppError> {
    let sessions: Vec<Session> = sqlx::query_as(
        "SELECT s.* FROM sessions s \
         JOIN session_members m ON m.session_id = s.id \
         WHERE m.user_id = ?",
    )
    .bind(&user_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(sessions))
}

pub async fn join_session(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(session_id): Path<String>,
) -> Result<Json<OkResponse>, AppError> {
    let exists: Option<(String,)> = sqlx::query_as("SELECT id FROM sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_optional(&state.pool)
        .await?;
    if exists.is_none() {
        return Err(AppError::NotFound("session not found".into()));
    }

    let now = Utc::now();
    sqlx::query(
        "INSERT OR IGNORE INTO session_members (session_id, user_id, role, joined_at) VALUES (?, ?, 'member', ?)",
    )
    .bind(&session_id)
    .bind(&user_id)
    .bind(now)
    .execute(&state.pool)
    .await?;

    Ok(Json(OkResponse { ok: true }))
}

// ---- Contracts ----

pub async fn create_contract(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(payload): Json<CreateContractRequest>,
) -> Result<Json<ContractResponse>, AppError> {
    require_member(&state, &payload.session_id, &user_id).await?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO contracts (id, session_id, owner_id, name, content, version, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?)",
    )
    .bind(&id)
    .bind(&payload.session_id)
    .bind(&user_id)
    .bind(&payload.name)
    .bind(&payload.content)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let contract = Contract {
        id,
        session_id: payload.session_id.clone(),
        owner_id: user_id.clone(),
        name: payload.name,
        content: payload.content,
        version: 1,
        created_at: now,
        updated_at: now,
    };

    broadcast_delta(
        &state,
        "contract_created",
        &payload.session_id,
        &user_id,
        serde_json::to_value(&contract).unwrap(),
    )
    .await;

    Ok(Json(ContractResponse { contract }))
}

pub async fn update_contract(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(contract_id): Path<String>,
    Json(payload): Json<UpdateContractRequest>,
) -> Result<Json<ContractResponse>, AppError> {
    let existing: Contract = sqlx::query_as("SELECT * FROM contracts WHERE id = ?")
        .bind(&contract_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("contract not found".into()))?;

    require_member(&state, &existing.session_id, &user_id).await?;

    let name = payload.name.unwrap_or(existing.name);
    let content = payload.content.unwrap_or(existing.content);
    let version = existing.version + 1;
    let now = Utc::now();

    sqlx::query(
        "UPDATE contracts SET name = ?, content = ?, version = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&name)
    .bind(&content)
    .bind(version)
    .bind(now)
    .bind(&contract_id)
    .execute(&state.pool)
    .await?;

    let contract = Contract {
        id: contract_id,
        session_id: existing.session_id.clone(),
        owner_id: existing.owner_id,
        name,
        content,
        version,
        created_at: existing.created_at,
        updated_at: now,
    };

    broadcast_delta(
        &state,
        "contract_updated",
        &existing.session_id,
        &user_id,
        serde_json::to_value(&contract).unwrap(),
    )
    .await;

    Ok(Json(ContractResponse { contract }))
}

pub async fn list_contracts(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Contract>>, AppError> {
    require_member(&state, &query.session_id, &user_id).await?;

    let contracts: Vec<Contract> = sqlx::query_as("SELECT * FROM contracts WHERE session_id = ?")
        .bind(&query.session_id)
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(contracts))
}

// ---- Intents ----

pub async fn create_intent(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(payload): Json<CreateIntentRequest>,
) -> Result<Json<IntentResponse>, AppError> {
    require_member(&state, &payload.session_id, &user_id).await?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO intents (id, session_id, user_id, description, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(&id)
    .bind(&payload.session_id)
    .bind(&user_id)
    .bind(&payload.description)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let intent = Intent {
        id,
        session_id: payload.session_id.clone(),
        user_id: user_id.clone(),
        description: payload.description,
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    };

    broadcast_delta(
        &state,
        "intent_created",
        &payload.session_id,
        &user_id,
        serde_json::to_value(&intent).unwrap(),
    )
    .await;

    Ok(Json(IntentResponse { intent }))
}

pub async fn update_intent(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(intent_id): Path<String>,
    Json(payload): Json<UpdateIntentRequest>,
) -> Result<Json<IntentResponse>, AppError> {
    let existing: Intent = sqlx::query_as("SELECT * FROM intents WHERE id = ?")
        .bind(&intent_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("intent not found".into()))?;

    require_member(&state, &existing.session_id, &user_id).await?;

    let description = payload.description.unwrap_or(existing.description);
    let status = payload.status.unwrap_or(existing.status);
    let now = Utc::now();

    sqlx::query("UPDATE intents SET description = ?, status = ?, updated_at = ? WHERE id = ?")
        .bind(&description)
        .bind(&status)
        .bind(now)
        .bind(&intent_id)
        .execute(&state.pool)
        .await?;

    let intent = Intent {
        id: intent_id,
        session_id: existing.session_id.clone(),
        user_id: existing.user_id,
        description,
        status,
        created_at: existing.created_at,
        updated_at: now,
    };

    broadcast_delta(
        &state,
        "intent_updated",
        &existing.session_id,
        &user_id,
        serde_json::to_value(&intent).unwrap(),
    )
    .await;

    Ok(Json(IntentResponse { intent }))
}

pub async fn list_intents(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<Intent>>, AppError> {
    require_member(&state, &query.session_id, &user_id).await?;

    let intents: Vec<Intent> = sqlx::query_as("SELECT * FROM intents WHERE session_id = ?")
        .bind(&query.session_id)
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(intents))
}

// ---- Execution Plans ----

pub async fn create_plan(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Json(payload): Json<CreatePlanRequest>,
) -> Result<Json<PlanResponse>, AppError> {
    require_member(&state, &payload.session_id, &user_id).await?;

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO execution_plans (id, session_id, author_id, content, status, created_at, updated_at) \
         VALUES (?, ?, ?, ?, 'pending', ?, ?)",
    )
    .bind(&id)
    .bind(&payload.session_id)
    .bind(&user_id)
    .bind(&payload.content)
    .bind(now)
    .bind(now)
    .execute(&state.pool)
    .await?;

    let plan = ExecutionPlan {
        id,
        session_id: payload.session_id.clone(),
        author_id: user_id.clone(),
        content: payload.content,
        status: "pending".to_string(),
        reviewed_by: None,
        review_note: None,
        created_at: now,
        updated_at: now,
    };

    state
        .hub
        .broadcast(
            &plan.session_id,
            &json!({
                "type": "plan_submitted",
                "data": serde_json::to_value(&plan).unwrap(),
                "author_id": user_id,
                "timestamp": now,
            }),
        )
        .await;

    Ok(Json(PlanResponse {
        status: plan.status.clone(),
        plan,
    }))
}

pub async fn approve_plan(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(plan_id): Path<String>,
) -> Result<Json<PlanResponse>, AppError> {
    let existing: ExecutionPlan = sqlx::query_as("SELECT * FROM execution_plans WHERE id = ?")
        .bind(&plan_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("plan not found".into()))?;

    require_session_owner(&state, &existing.session_id, &user_id).await?;

    let now = Utc::now();
    sqlx::query(
        "UPDATE execution_plans SET status = 'approved', reviewed_by = ?, review_note = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(&user_id)
    .bind(now)
    .bind(&plan_id)
    .execute(&state.pool)
    .await?;

    let plan = ExecutionPlan {
        status: "approved".to_string(),
        reviewed_by: Some(user_id.clone()),
        review_note: None,
        updated_at: now,
        ..existing
    };

    state
        .hub
        .broadcast(
            &plan.session_id,
            &json!({
                "type": "plan_approved",
                "plan_id": plan.id,
                "reviewed_by": user_id,
                "timestamp": now,
            }),
        )
        .await;

    Ok(Json(PlanResponse {
        status: plan.status.clone(),
        plan,
    }))
}

pub async fn reject_plan(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Path(plan_id): Path<String>,
    Json(payload): Json<RejectPlanRequest>,
) -> Result<Json<PlanResponse>, AppError> {
    let existing: ExecutionPlan = sqlx::query_as("SELECT * FROM execution_plans WHERE id = ?")
        .bind(&plan_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("plan not found".into()))?;

    require_session_owner(&state, &existing.session_id, &user_id).await?;

    let now = Utc::now();
    sqlx::query(
        "UPDATE execution_plans SET status = 'rejected', reviewed_by = ?, review_note = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&user_id)
    .bind(&payload.note)
    .bind(now)
    .bind(&plan_id)
    .execute(&state.pool)
    .await?;

    let plan = ExecutionPlan {
        status: "rejected".to_string(),
        reviewed_by: Some(user_id.clone()),
        review_note: payload.note.clone(),
        updated_at: now,
        ..existing
    };

    state
        .hub
        .broadcast(
            &plan.session_id,
            &json!({
                "type": "plan_rejected",
                "plan_id": plan.id,
                "reviewed_by": user_id,
                "note": payload.note,
                "timestamp": now,
            }),
        )
        .await;

    Ok(Json(PlanResponse {
        status: plan.status.clone(),
        plan,
    }))
}

pub async fn list_plans(
    State(state): State<AppState>,
    Extension(AuthUser(user_id)): Extension<AuthUser>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<ExecutionPlan>>, AppError> {
    require_member(&state, &query.session_id, &user_id).await?;

    let plans: Vec<ExecutionPlan> =
        sqlx::query_as("SELECT * FROM execution_plans WHERE session_id = ?")
            .bind(&query.session_id)
            .fetch_all(&state.pool)
            .await?;

    Ok(Json(plans))
}
