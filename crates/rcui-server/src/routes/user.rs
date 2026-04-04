use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db;
use crate::error::AppError;
use crate::state::AppState;

// ─── Git Config ──────────────────────────────────────────────────────────────

pub async fn get_git_config(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let git_config = db::users::get_by_id(&state.db, auth.user_id).await?;

    let (git_name, git_email) = match git_config {
        Some(u) => {
            let name = u.git_name.clone();
            let email = u.git_email.clone();

            // Auto-populate from system git config if DB is empty
            if name.is_none() && email.is_none() {
                let system = get_system_git_config().await;
                if system.0.is_some() || system.1.is_some() {
                    db::users::update_git_config(
                        &state.db,
                        auth.user_id,
                        system.0.as_deref(),
                        system.1.as_deref(),
                    )
                    .await?;
                }
                system
            } else {
                (name, email)
            }
        }
        None => (None, None),
    };

    Ok(Json(json!({
        "success": true,
        "gitName": git_name,
        "gitEmail": git_email
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateGitConfigRequest {
    pub git_name: String,
    pub git_email: String,
}

pub async fn update_git_config(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpdateGitConfigRequest>,
) -> Result<Json<Value>, AppError> {
    if body.git_name.is_empty() || body.git_email.is_empty() {
        return Err(AppError::BadRequest(
            "Git name and email are required".to_string(),
        ));
    }

    // Basic email validation
    if !body.git_email.contains('@') || !body.git_email.contains('.') {
        return Err(AppError::BadRequest("Invalid email format".to_string()));
    }

    db::users::update_git_config(&state.db, auth.user_id, Some(&body.git_name), Some(&body.git_email))
        .await?;

    // Try to apply globally (best-effort)
    let name = body.git_name.clone();
    let email = body.git_email.clone();
    tokio::spawn(async move {
        let _ = tokio::process::Command::new("git")
            .args(["config", "--global", "user.name", &name])
            .output()
            .await;
        let _ = tokio::process::Command::new("git")
            .args(["config", "--global", "user.email", &email])
            .output()
            .await;
    });

    Ok(Json(json!({
        "success": true,
        "gitName": body.git_name,
        "gitEmail": body.git_email
    })))
}

// ─── Onboarding ──────────────────────────────────────────────────────────────

pub async fn complete_onboarding(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    db::users::complete_onboarding(&state.db, auth.user_id).await?;
    Ok(Json(json!({
        "success": true,
        "message": "Onboarding completed successfully"
    })))
}

pub async fn get_onboarding_status(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let u = db::users::get_by_id(&state.db, auth.user_id).await?;
    let completed = u.map(|u| u.has_completed_onboarding).unwrap_or(false);
    Ok(Json(json!({
        "success": true,
        "hasCompletedOnboarding": completed
    })))
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

async fn get_system_git_config() -> (Option<String>, Option<String>) {
    let name = tokio::process::Command::new("git")
        .args(["config", "--global", "user.name"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let email = tokio::process::Command::new("git")
        .args(["config", "--global", "user.email"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    (name, email)
}
