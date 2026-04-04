use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info};

use crate::auth::middleware::AuthUser;
use crate::db;
use crate::error::AppError;
use crate::providers::claude::adapter::ClaudeAdapter;
use crate::providers::codex::adapter::CodexAdapter;
use crate::providers::cursor::adapter::CursorAdapter;
use crate::providers::gemini::adapter::GeminiAdapter;
use crate::providers::types::{FetchHistoryOptions, SessionProvider};
use crate::providers::ProviderAdapter;
use crate::services::project_scanner;
use crate::state::AppState;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsQuery {
    pub provider: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// GET /api/projects/:projectName/sessions — List sessions for a project.
pub async fn list_sessions(
    _auth: AuthUser,
    State(_state): State<Arc<AppState>>,
    Path(project_name): Path<String>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<Value>, AppError> {
    let limit = query.limit.map(|l| l as usize);
    let offset = query.offset.unwrap_or(0) as usize;

    debug!(%project_name, ?limit, offset, "Listing sessions");

    let (sessions, total, has_more) =
        project_scanner::get_claude_sessions(&project_name, limit, offset).await;

    debug!(%project_name, total, has_more, "Sessions listed");

    Ok(Json(json!({
        "sessions": sessions,
        "total": total,
        "hasMore": has_more
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessagesQuery {
    pub provider: Option<String>,
    pub project_name: Option<String>,
    pub project_path: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// GET /api/sessions/:sessionId/messages — Get messages for a session.
pub async fn get_session_messages(
    _auth: AuthUser,
    State(_state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Result<Json<Value>, AppError> {
    let provider_str = query.provider.as_deref().unwrap_or("claude");
    let provider: SessionProvider = provider_str
        .parse()
        .map_err(|e: String| AppError::BadRequest(e))?;

    debug!(%session_id, %provider_str, "Fetching session messages");

    let opts = FetchHistoryOptions {
        project_name: query.project_name.clone(),
        project_path: query.project_path.clone(),
        limit: query.limit,
        offset: query.offset.unwrap_or(0),
    };

    let result = match provider {
        SessionProvider::Claude => ClaudeAdapter.fetch_history(&session_id, opts).await?,
        SessionProvider::Cursor => CursorAdapter.fetch_history(&session_id, opts).await?,
        SessionProvider::Codex => CodexAdapter.fetch_history(&session_id, opts).await?,
        SessionProvider::Gemini => GeminiAdapter.fetch_history(&session_id, opts).await?,
    };

    debug!(
        %session_id,
        %provider_str,
        total = result.total,
        returned = result.messages.len(),
        "Session messages fetched"
    );

    Ok(Json(serde_json::to_value(&result).unwrap_or_default()))
}

/// DELETE /api/sessions/:sessionId — Delete a session.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteSessionQuery {
    pub provider: Option<String>,
    pub project_name: Option<String>,
}

pub async fn delete_session(
    _auth: AuthUser,
    State(_state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<DeleteSessionQuery>,
) -> Result<Json<Value>, AppError> {
    let provider = query.provider.as_deref().unwrap_or("claude");
    let project_name = query.project_name.as_deref().unwrap_or("");

    info!(%session_id, %provider, "Deleting session");

    project_scanner::delete_session(project_name, &session_id, provider).await?;

    Ok(Json(json!({ "success": true })))
}

/// POST /api/sessions/:sessionId/name — Set custom session name.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetNameRequest {
    pub name: String,
    pub provider: Option<String>,
}

pub async fn set_session_name(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(body): Json<SetNameRequest>,
) -> Result<Json<Value>, AppError> {
    let provider = body.provider.as_deref().unwrap_or("claude");
    debug!(%session_id, %provider, name = %body.name, "Setting session name");
    db::session_names::set_name(&state.db, &session_id, provider, &body.name).await?;
    Ok(Json(json!({ "success": true })))
}

/// DELETE /api/sessions/:sessionId/name — Delete custom session name.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteNameQuery {
    pub provider: Option<String>,
}

pub async fn delete_session_name(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<DeleteNameQuery>,
) -> Result<Json<Value>, AppError> {
    let provider = query.provider.as_deref().unwrap_or("claude");
    debug!(%session_id, %provider, "Deleting session name");
    db::session_names::delete_name(&state.db, &session_id, provider).await?;
    Ok(Json(json!({ "success": true })))
}
