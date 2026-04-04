use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::middleware::AuthUser;
use crate::db;
use crate::error::AppError;
use crate::state::AppState;

// ─── API Keys ────────────────────────────────────────────────────────────────

pub async fn list_api_keys(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let keys = db::api_keys::get_all_for_user(&state.db, auth.user_id).await?;
    let sanitized: Vec<Value> = keys
        .iter()
        .map(|k| {
            let mut v = serde_json::to_value(k).unwrap_or_default();
            if let Some(key) = v.get_mut("api_key").and_then(|v| v.as_str().map(|s| s.to_string()))
            {
                v["api_key"] = Value::String(format!("{}...", &key[..key.len().min(10)]));
            }
            v
        })
        .collect();
    Ok(Json(json!({ "apiKeys": sanitized })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApiKeyRequest {
    pub key_name: String,
}

pub async fn create_api_key(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<Json<Value>, AppError> {
    let name = body.key_name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Key name is required".to_string()));
    }
    let key = db::api_keys::create(&state.db, auth.user_id, name).await?;
    Ok(Json(json!({ "success": true, "apiKey": key })))
}

pub async fn delete_api_key(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(key_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let deleted = db::api_keys::delete(&state.db, key_id, auth.user_id).await?;
    if deleted {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(AppError::NotFound("API key not found".to_string()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToggleRequest {
    pub is_active: bool,
}

pub async fn toggle_api_key(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(key_id): Path<i64>,
    Json(body): Json<ToggleRequest>,
) -> Result<Json<Value>, AppError> {
    let toggled = db::api_keys::toggle(&state.db, key_id, auth.user_id, body.is_active).await?;
    if toggled {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(AppError::NotFound("API key not found".to_string()))
    }
}

// ─── Credentials ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CredentialQuery {
    #[serde(rename = "type")]
    pub cred_type: Option<String>,
}

pub async fn list_credentials(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Query(query): Query<CredentialQuery>,
) -> Result<Json<Value>, AppError> {
    let creds =
        db::credentials::get_all(&state.db, auth.user_id, query.cred_type.as_deref()).await?;
    Ok(Json(json!({ "credentials": creds })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCredentialRequest {
    pub credential_name: String,
    pub credential_type: String,
    pub credential_value: String,
    pub description: Option<String>,
}

pub async fn create_credential(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateCredentialRequest>,
) -> Result<Json<Value>, AppError> {
    if body.credential_name.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Credential name is required".to_string(),
        ));
    }
    if body.credential_type.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Credential type is required".to_string(),
        ));
    }
    if body.credential_value.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Credential value is required".to_string(),
        ));
    }
    let cred = db::credentials::create(
        &state.db,
        auth.user_id,
        body.credential_name.trim(),
        body.credential_type.trim(),
        body.credential_value.trim(),
        body.description.as_deref().map(|s| s.trim()),
    )
    .await?;
    Ok(Json(json!({ "success": true, "credential": cred })))
}

pub async fn delete_credential(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(credential_id): Path<i64>,
) -> Result<Json<Value>, AppError> {
    let deleted = db::credentials::delete(&state.db, credential_id, auth.user_id).await?;
    if deleted {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(AppError::NotFound("Credential not found".to_string()))
    }
}

pub async fn toggle_credential(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Path(credential_id): Path<i64>,
    Json(body): Json<ToggleRequest>,
) -> Result<Json<Value>, AppError> {
    let toggled =
        db::credentials::toggle(&state.db, credential_id, auth.user_id, body.is_active).await?;
    if toggled {
        Ok(Json(json!({ "success": true })))
    } else {
        Err(AppError::NotFound("Credential not found".to_string()))
    }
}

// ─── Notification Preferences ────────────────────────────────────────────────

pub async fn get_notification_preferences(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let prefs = db::notifications::get_preferences(&state.db, auth.user_id).await?;
    Ok(Json(json!({ "success": true, "preferences": prefs })))
}

pub async fn update_notification_preferences(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let prefs_json = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    let prefs = db::notifications::update_preferences(&state.db, auth.user_id, &prefs_json).await?;
    Ok(Json(json!({ "success": true, "preferences": prefs })))
}

// ─── Push Subscriptions ──────────────────────────────────────────────────────

pub async fn get_vapid_public_key(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Value>, AppError> {
    let keys = db::push_subscriptions::get_or_create_vapid_keys(&state.db).await?;
    Ok(Json(json!({ "publicKey": keys.0 })))
}

#[derive(Deserialize)]
pub struct SubscribeRequest {
    pub endpoint: String,
    pub keys: SubscriptionKeys,
}

#[derive(Deserialize)]
pub struct SubscriptionKeys {
    pub p256dh: String,
    pub auth: String,
}

pub async fn push_subscribe(
    auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<SubscribeRequest>,
) -> Result<Json<Value>, AppError> {
    if body.endpoint.is_empty() || body.keys.p256dh.is_empty() || body.keys.auth.is_empty() {
        return Err(AppError::BadRequest(
            "Missing subscription fields".to_string(),
        ));
    }
    db::push_subscriptions::save(
        &state.db,
        auth.user_id,
        &body.endpoint,
        &body.keys.p256dh,
        &body.keys.auth,
    )
    .await?;
    Ok(Json(json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct UnsubscribeRequest {
    pub endpoint: String,
}

pub async fn push_unsubscribe(
    _auth: AuthUser,
    State(state): State<Arc<AppState>>,
    Json(body): Json<UnsubscribeRequest>,
) -> Result<Json<Value>, AppError> {
    if body.endpoint.is_empty() {
        return Err(AppError::BadRequest("Missing endpoint".to_string()));
    }
    db::push_subscriptions::remove(&state.db, &body.endpoint).await?;
    Ok(Json(json!({ "success": true })))
}
