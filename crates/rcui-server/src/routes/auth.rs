use std::sync::Arc;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use crate::auth::jwt;
use crate::auth::middleware::AuthUser;
use crate::auth::password;
use crate::db;
use crate::error::AppError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct AuthRequest {
    pub username: String,
    pub password: String,
}

/// GET /api/auth/status — Check whether setup is needed.
pub async fn status(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let has_users = db::users::has_users(&state.db).await?;
    debug!(has_users, "Auth status checked");
    Ok(Json(json!({
        "needsSetup": !has_users,
        "isAuthenticated": false,
    })))
}

/// POST /api/auth/register — Register the first user.
pub async fn register(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthRequest>,
) -> Result<Json<Value>, AppError> {
    info!(username = %body.username, "User registration attempt");

    // Only allow registration if no users exist (single-user system)
    let has_users = db::users::has_users(&state.db).await?;
    if has_users {
        warn!("Registration rejected: user already exists");
        return Err(AppError::Conflict(
            "A user is already registered. Only one user is allowed.".to_string(),
        ));
    }

    if body.username.trim().is_empty() || body.password.is_empty() {
        return Err(AppError::BadRequest(
            "Username and password are required".to_string(),
        ));
    }

    let hash = password::hash_password(&body.password)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to hash password: {e}")))?;

    let user = db::users::create_user(&state.db, body.username.trim(), &hash).await?;
    info!(user_id = user.id, username = %user.username, "User registered successfully");

    let token = jwt::generate_token(&state.jwt_secret, user.id, &user.username)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to generate token: {e}")))?;

    Ok(Json(json!({
        "success": true,
        "user": {
            "id": user.id,
            "username": user.username,
        },
        "token": token,
    })))
}

/// POST /api/auth/login — Log in with username and password.
pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuthRequest>,
) -> Result<(HeaderMap, Json<Value>), AppError> {
    info!(username = %body.username, "Login attempt");

    let user = db::users::get_by_username(&state.db, &body.username)
        .await?
        .ok_or_else(|| AppError::Unauthorized("Invalid username or password".to_string()))?;

    let valid = password::verify_password(&body.password, &user.password_hash)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Password verification failed: {e}")))?;

    if !valid {
        warn!(username = %body.username, "Login failed: invalid password");
        return Err(AppError::Unauthorized(
            "Invalid username or password".to_string(),
        ));
    }

    db::users::update_last_login(&state.db, user.id).await?;

    let token = jwt::generate_token(&state.jwt_secret, user.id, &user.username)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("Failed to generate token: {e}")))?;

    info!(user_id = user.id, username = %user.username, "Login successful");

    Ok((
        HeaderMap::new(),
        Json(json!({
            "success": true,
            "user": {
                "id": user.id,
                "username": user.username,
            },
            "token": token,
        })),
    ))
}

/// GET /api/auth/user — Get authenticated user info.
pub async fn user_info(auth: AuthUser) -> Json<Value> {
    Json(json!({
        "user": {
            "id": auth.user_id,
            "username": auth.username,
        }
    }))
}

/// POST /api/auth/logout — Logout (client-side token removal).
pub async fn logout() -> Json<Value> {
    Json(json!({
        "success": true,
        "message": "Logged out successfully",
    }))
}
